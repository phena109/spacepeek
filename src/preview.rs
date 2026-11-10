use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeniedKind {
    /// Raster image. Later preview work can match this without a second list.
    Image,
    /// Executable, archive, document, media, or other container.
    Binary,
}

/// How many entries a directory listing produced.
///
/// [`DirectoryCount::Capped`] means the listing stopped after [`DIRECTORY_COUNT_CAP`]
/// so a huge folder cannot be walked without a bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryCount {
    Exact(u64),
    Capped,
}

/// Stop counting directory entries after this many. One extra entry is read
/// so an exact cap and "cap+" stay distinct.
pub const DIRECTORY_COUNT_CAP: u64 = 10_000;

#[derive(Debug)]
pub enum PreviewError {
    /// Content sniff failed (NUL or invalid UTF-8 in the sniff window).
    Binary,
    /// Extension is on the deny-list. The kind says which family it is.
    Denied(DeniedKind),
    /// A directory. `count` is omitted when the entries could not be read.
    Directory { count: Option<DirectoryCount> },
    /// Zero-byte file. Distinct from a preview that has not loaded yet.
    Empty,
    Cancelled,
    Io(std::io::Error),
    /// `.lnk` shortcut. `target` is the stored path when it could be read
    /// without searching or showing UI. Resolution itself is Win32 glue.
    Shortcut { target: Option<String> },
}

impl PartialEq for PreviewError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (PreviewError::Io(a), PreviewError::Io(b)) => {
                a.kind() == b.kind() && a.to_string() == b.to_string()
            }
            (PreviewError::Denied(a), PreviewError::Denied(b)) => a == b,
            (PreviewError::Directory { count: a }, PreviewError::Directory { count: b }) => a == b,
            (PreviewError::Shortcut { target: a }, PreviewError::Shortcut { target: b }) => a == b,
            (PreviewError::Binary, PreviewError::Binary)
            | (PreviewError::Empty, PreviewError::Empty)
            | (PreviewError::Cancelled, PreviewError::Cancelled) => true,
            _ => false,
        }
    }
}

impl Eq for PreviewError {}

impl std::fmt::Display for PreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PreviewError::Binary => write!(f, "file looks binary"),
            PreviewError::Denied(DeniedKind::Image) => write!(f, "image file"),
            PreviewError::Denied(DeniedKind::Binary) => write!(f, "unsupported file type"),
            PreviewError::Directory { .. } => write!(f, "is a directory"),
            PreviewError::Empty => write!(f, "empty file"),
            PreviewError::Cancelled => write!(f, "cancelled"),
            PreviewError::Io(e) => write!(f, "io error: {e}"),
            PreviewError::Shortcut { .. } => write!(f, "shortcut"),
        }
    }
}

impl PreviewError {
    /// Sentence for a person, including the file name where it helps.
    ///
    /// [`Display`] stays the short technical form. Callers that draw the
    /// preview use this instead.
    pub fn user_message(&self, file_name: &str) -> String {
        let name = display_name(file_name);
        match self {
            PreviewError::Binary => format!("{name} doesn't look like text"),
            PreviewError::Denied(_) => {
                format!("{name} can't be previewed — this file type isn't supported yet")
            }
            PreviewError::Directory { count } => format_directory_message(name, *count),
            PreviewError::Empty => "This file is empty".to_string(),
            PreviewError::Cancelled => "Preview cancelled".to_string(),
            PreviewError::Shortcut { target: Some(target) } if !target.trim().is_empty() => {
                format!("Shortcut to {target}")
            }
            PreviewError::Shortcut { .. } => {
                "Shortcut — previewing shortcuts isn't supported yet".to_string()
            }
            PreviewError::Io(err) => io_user_message(name, err),
        }
    }

    /// Short second line for the header. The centred body uses [`Self::user_message`].
    pub fn header_status(&self) -> String {
        match self {
            PreviewError::Binary => "Not text".to_string(),
            PreviewError::Denied(_) => "Not supported".to_string(),
            PreviewError::Directory { count } => match count {
                Some(count) => format_directory_count(*count),
                None => "Folder".to_string(),
            },
            PreviewError::Empty => format_compact_size(0),
            PreviewError::Cancelled => "Cancelled".to_string(),
            PreviewError::Shortcut { .. } => "Shortcut".to_string(),
            PreviewError::Io(err) => io_header_status(err).to_string(),
        }
    }
}

fn display_name(name: &str) -> &str {
    if name.is_empty() {
        "This file"
    } else {
        name
    }
}

/// `Folder: photos` or `Folder: photos · 12 items`. The count is omitted when
/// the directory could not be listed. Above [`DIRECTORY_COUNT_CAP`] the count
/// is `10,000+`.
pub fn format_directory_message(name: &str, count: Option<DirectoryCount>) -> String {
    let name = display_name(name);
    match count {
        None => format!("Folder: {name}"),
        Some(count) => format!("Folder: {name} · {}", format_directory_count(count)),
    }
}

fn format_directory_count(count: DirectoryCount) -> String {
    match count {
        DirectoryCount::Exact(n) => format_item_count(n),
        DirectoryCount::Capped => format!("{}+ items", format_grouped(DIRECTORY_COUNT_CAP)),
    }
}

fn format_item_count(n: u64) -> String {
    let grouped = format_grouped(n);
    if n == 1 {
        format!("{grouped} item")
    } else {
        format!("{grouped} items")
    }
}

#[derive(Clone, Copy)]
enum IoClass {
    Sharing,
    NotFound,
    Denied,
    Other,
}

fn io_class(err: &std::io::Error) -> IoClass {
    // Windows codes, compared on the raw value so a constructed error is
    // classified the same on every host. Kind is the fallback for errors that
    // `std` built without a raw code.
    match err.raw_os_error() {
        Some(32 | 33) => return IoClass::Sharing,
        Some(2 | 3) => return IoClass::NotFound,
        Some(5) => return IoClass::Denied,
        _ => {}
    }
    match err.kind() {
        std::io::ErrorKind::NotFound => IoClass::NotFound,
        std::io::ErrorKind::PermissionDenied => IoClass::Denied,
        _ => IoClass::Other,
    }
}

fn io_user_message(name: &str, err: &std::io::Error) -> String {
    match io_class(err) {
        IoClass::Sharing => format!("{name} is open in another program and can't be read"),
        IoClass::NotFound => format!("{name} no longer exists"),
        IoClass::Denied => format!("Access to {name} was denied"),
        IoClass::Other => format!("{name} can't be read"),
    }
}

fn io_header_status(err: &std::io::Error) -> &'static str {
    match io_class(err) {
        IoClass::Sharing => "In use",
        IoClass::NotFound => "Missing",
        IoClass::Denied => "Access denied",
        IoClass::Other => "Can't read",
    }
}

impl std::fmt::Display for DeniedKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeniedKind::Image => write!(f, "image file"),
            DeniedKind::Binary => write!(f, "unsupported file type"),
        }
    }
}

impl From<std::io::Error> for PreviewError {
    fn from(e: std::io::Error) -> Self {
        PreviewError::Io(e)
    }
}

/// How a preview was shortened. Absent when the whole file is shown unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Truncation {
    /// File bytes represented by the preview, after backing off a split character.
    /// Includes a leading BOM when the file has one.
    pub bytes_shown: u64,
    pub total_bytes: u64,
    pub lines_shown: u64,
    /// Line count of the whole file. `None` when the byte budget stopped the read,
    /// because the rest of the file was never counted.
    pub total_lines: Option<u64>,
    /// Some decoded lines were dropped because of the line cap.
    pub lines_omitted: bool,
    /// At least one shown line was cut to [`MAX_LINE_CHARS`] and marked with `…`.
    pub lines_shortened: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    /// Text for the EDIT control: CRLF endings, long lines already capped.
    pub content: String,
    pub truncated: bool,
    pub line_count: usize,
    pub truncation: Option<Truncation>,
    /// File size in bytes, including a leading BOM. The header uses this.
    pub total_bytes: u64,
}

/// Default file-byte budget for the EDIT control.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;
/// Default cap on the number of lines placed in the control.
pub const DEFAULT_MAX_LINES: usize = 2_000;
/// Maximum Unicode scalar values kept from one source line, before the `…` marker.
pub const MAX_LINE_CHARS: usize = 2_000;
/// How many leading bytes the text sniff inspects.
pub const BINARY_SNIFF_BYTES: usize = 8192;

#[derive(Debug, Clone)]
pub struct PreviewLimits {
    pub max_bytes: usize,
    pub max_lines: usize,
}

impl Default for PreviewLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            max_lines: DEFAULT_MAX_LINES,
        }
    }
}

impl PreviewLimits {
    pub fn new(max_bytes: usize, max_lines: usize) -> Self {
        Self {
            max_bytes,
            max_lines,
        }
    }
}

/// Known non-text extensions. Add a row to refuse that extension before the sniff.
/// Image rows stay separate so a later preview can take them over.
const DENIED_EXTENSIONS: &[(&str, DeniedKind)] = &[
    ("png", DeniedKind::Image),
    ("jpg", DeniedKind::Image),
    ("jpeg", DeniedKind::Image),
    ("gif", DeniedKind::Image),
    ("bmp", DeniedKind::Image),
    ("webp", DeniedKind::Image),
    ("ico", DeniedKind::Image),
    ("exe", DeniedKind::Binary),
    ("dll", DeniedKind::Binary),
    ("sys", DeniedKind::Binary),
    ("com", DeniedKind::Binary),
    ("scr", DeniedKind::Binary),
    ("ocx", DeniedKind::Binary),
    ("msi", DeniedKind::Binary),
    ("zip", DeniedKind::Binary),
    ("7z", DeniedKind::Binary),
    ("rar", DeniedKind::Binary),
    ("gz", DeniedKind::Binary),
    ("tgz", DeniedKind::Binary),
    ("bz2", DeniedKind::Binary),
    ("xz", DeniedKind::Binary),
    ("tar", DeniedKind::Binary),
    ("cab", DeniedKind::Binary),
    ("iso", DeniedKind::Binary),
    ("jar", DeniedKind::Binary),
    ("wasm", DeniedKind::Binary),
    ("docx", DeniedKind::Binary),
    ("xlsx", DeniedKind::Binary),
    ("pptx", DeniedKind::Binary),
    ("doc", DeniedKind::Binary),
    ("xls", DeniedKind::Binary),
    ("ppt", DeniedKind::Binary),
    ("pdf", DeniedKind::Binary),
    ("mp3", DeniedKind::Binary),
    ("mp4", DeniedKind::Binary),
    ("m4a", DeniedKind::Binary),
    ("aac", DeniedKind::Binary),
    ("wav", DeniedKind::Binary),
    ("flac", DeniedKind::Binary),
    ("ogg", DeniedKind::Binary),
    ("avi", DeniedKind::Binary),
    ("mkv", DeniedKind::Binary),
    ("mov", DeniedKind::Binary),
    ("webm", DeniedKind::Binary),
];

/// Deny-list hit for `path`, if any. Comparison is case-insensitive.
/// Extensionless names and dotfiles (`.gitignore`) are not denied.
pub fn denied_kind(path: &Path) -> Option<DeniedKind> {
    let ext = path.extension()?.to_str()?;
    DENIED_EXTENSIONS.iter().find_map(|(name, kind)| {
        if name.eq_ignore_ascii_case(ext) {
            Some(*kind)
        } else {
            None
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation {
    pub value: u64,
}

impl Generation {
    pub fn new(value: u64) -> Self {
        Self { value }
    }
}

#[derive(Debug, Clone)]
pub struct CancelToken {
    next_gen: Arc<AtomicU64>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            next_gen: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn generate(&self) -> Generation {
        Generation {
            value: self.next_gen.fetch_add(1, Ordering::SeqCst),
        }
    }

    pub fn current_generation(&self) -> Generation {
        Generation {
            value: self.next_gen.load(Ordering::SeqCst).saturating_sub(1),
        }
    }

    pub fn is_cancelled(&self, gen: &Generation) -> bool {
        self.next_gen.load(Ordering::SeqCst) > gen.value.saturating_add(1)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

pub fn load_text_preview<P: AsRef<Path>>(
    path: P,
    limits: PreviewLimits,
    cancel: &CancelToken,
    gen: Generation,
) -> Result<Preview, PreviewError> {
    let path = path.as_ref();
    if path.is_dir() {
        if cancel.is_cancelled(&gen) {
            return Err(PreviewError::Cancelled);
        }
        return Err(PreviewError::Directory {
            count: directory_count(path),
        });
    }
    // Shortcuts are refused before the byte sniff. The stored target, when one
    // can be read cheaply, is filled in by the Windows caller.
    if is_shortcut(path) {
        return Err(PreviewError::Shortcut { target: None });
    }
    if let Some(kind) = denied_kind(path) {
        return Err(PreviewError::Denied(kind));
    }
    if cancel.is_cancelled(&gen) {
        return Err(PreviewError::Cancelled);
    }

    let mut file = File::open(path).map_err(PreviewError::Io)?;
    let mut total_bytes = file.metadata().map_err(PreviewError::Io)?.len();
    if total_bytes == 0 {
        return Err(PreviewError::Empty);
    }
    let mut reader = BufReader::new(&mut file);
    let mut buffer: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];

    loop {
        if cancel.is_cancelled(&gen) {
            return Err(PreviewError::Cancelled);
        }
        if buffer.len() >= limits.max_bytes {
            break;
        }
        let remaining = (limits.max_bytes - buffer.len()).min(chunk.len());
        let n = reader.read(&mut chunk[..remaining])?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    if (buffer.len() as u64) > total_bytes {
        total_bytes = buffer.len() as u64;
    }
    let hit_byte_cap = (buffer.len() as u64) < total_bytes;

    // UTF-16 BOM must be recognised before the NUL sniff: ASCII in UTF-16 is
    // full of 0x00 bytes. UTF-32LE is FF FE 00 00; we do not support UTF-32
    // and decode that prefix as UTF-16LE.
    let (text, bytes_shown) = if buffer.starts_with(&[0xFF, 0xFE]) {
        decode_utf16_prefix(&buffer[2..], true, hit_byte_cap, 2)
    } else if buffer.starts_with(&[0xFE, 0xFF]) {
        decode_utf16_prefix(&buffer[2..], false, hit_byte_cap, 2)
    } else if !passes_text_sniff(&buffer, total_bytes) {
        return Err(PreviewError::Binary);
    } else {
        decode_utf8_prefix(&buffer, hit_byte_cap)
    };

    let prepared = prepare_edit_text(&text, limits.max_lines);
    let fully_read = bytes_shown >= total_bytes;
    let total_lines = if fully_read {
        Some(prepared.lines_in_source as u64)
    } else {
        None
    };
    let byte_cut = bytes_shown < total_bytes;
    let truncated = byte_cut || prepared.lines_omitted || prepared.lines_shortened;
    let truncation = if truncated {
        Some(Truncation {
            bytes_shown,
            total_bytes,
            lines_shown: prepared.lines_shown as u64,
            total_lines,
            lines_omitted: prepared.lines_omitted,
            lines_shortened: prepared.lines_shortened,
        })
    } else {
        None
    };

    Ok(Preview {
        content: prepared.content,
        truncated,
        line_count: prepared.lines_shown,
        truncation,
        total_bytes,
    })
}

fn is_shortcut(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("lnk"))
}

/// `read_dir` does not yield `.` or `..`. A listing error on one entry still
/// counts, because something is there. `None` means the directory itself
/// could not be opened.
fn directory_count(path: &Path) -> Option<DirectoryCount> {
    directory_count_capped(path, DIRECTORY_COUNT_CAP)
}

fn directory_count_capped(path: &Path, cap: u64) -> Option<DirectoryCount> {
    let entries = std::fs::read_dir(path).ok()?;
    let mut seen = 0u64;
    for _entry in entries {
        seen = seen.saturating_add(1);
        if seen > cap {
            return Some(DirectoryCount::Capped);
        }
    }
    Some(DirectoryCount::Exact(seen))
}

/// Text placed in the preview control, with the truncation notice at the top.
pub fn preview_text(preview: &Preview) -> String {
    let Some(info) = preview.truncation.as_ref() else {
        return preview.content.clone();
    };
    let Some(notice) = format_truncation_notice(info) else {
        return preview.content.clone();
    };
    if preview.content.is_empty() {
        notice
    } else {
        let mut body = String::with_capacity(notice.len() + 4 + preview.content.len());
        body.push_str(&notice);
        body.push_str("\r\n\r\n");
        body.push_str(&preview.content);
        body
    }
}

/// Human-readable truncation line, such as `Showing the first 64 KB of 1.2 MB`.
///
/// Sizes use binary units (1024). An exact multiple prints as `64 KB`; anything
/// else prints one decimal digit (`1.2 MB`). Returns `None` when nothing was cut.
pub fn format_truncation_notice(info: &Truncation) -> Option<String> {
    let byte_cut = info.bytes_shown < info.total_bytes;
    let known_lines = info.total_lines.filter(|total| *total > info.lines_shown);

    let mut notice = String::new();
    if byte_cut {
        notice.push_str("Showing the first ");
        notice.push_str(&format_byte_size(info.bytes_shown));
        notice.push_str(" of ");
        notice.push_str(&format_byte_size(info.total_bytes));
        if info.lines_omitted {
            notice.push_str(" (");
            notice.push_str(&format_grouped(info.lines_shown));
            if let Some(total) = known_lines {
                notice.push_str(" of ");
                notice.push_str(&format_grouped(total));
            }
            notice.push_str(" lines)");
        }
    } else if let Some(total) = known_lines {
        notice.push_str("Showing the first ");
        notice.push_str(&format_grouped(info.lines_shown));
        notice.push_str(" of ");
        notice.push_str(&format_grouped(total));
        notice.push_str(" lines");
    }

    if info.lines_shortened {
        if notice.is_empty() {
            notice.push_str("Some lines were shortened.");
        } else {
            notice.push_str(". Some lines were shortened.");
        }
    }

    if notice.is_empty() {
        None
    } else {
        Some(notice)
    }
}

/// Binary-unit size. `1` is `1 byte`; values under 1024 are `N bytes`.
pub fn format_byte_size(bytes: u64) -> String {
    if bytes < 1024 {
        return if bytes == 1 {
            "1 byte".to_string()
        } else {
            format!("{bytes} bytes")
        };
    }
    let kb = 1024u64;
    let mb = kb * 1024;
    let gb = mb * 1024;
    let tb = gb * 1024;
    let (unit, name) = if bytes < mb {
        (kb, "KB")
    } else if bytes < gb {
        (mb, "MB")
    } else if bytes < tb {
        (gb, "GB")
    } else {
        (tb, "TB")
    };
    format_scaled(bytes, unit, name)
}

/// Compact binary size for the header: `512 B`, `12 KB`, `1.5 MB`.
///
/// Same half-up tenths rounding as [`format_byte_size`]. Values under 1024 use
/// `B` so the header line stays short.
pub fn format_compact_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let kb = 1024u64;
    let mb = kb * 1024;
    let gb = mb * 1024;
    let tb = gb * 1024;
    let (unit, name) = if bytes < mb {
        (kb, "KB")
    } else if bytes < gb {
        (mb, "MB")
    } else if bytes < tb {
        (gb, "GB")
    } else {
        (tb, "TB")
    };
    format_scaled(bytes, unit, name)
}

fn format_scaled(bytes: u64, unit: u64, name: &str) -> String {
    let tenths = (u128::from(bytes) * 10 + u128::from(unit) / 2) / u128::from(unit);
    let whole = tenths / 10;
    let frac = tenths % 10;
    if frac == 0 {
        format!("{whole} {name}")
    } else {
        format!("{whole}.{frac} {name}")
    }
}

pub(crate) fn format_grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Edit-control text: each source line capped on a char boundary, joined with CRLF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedText {
    pub content: String,
    pub lines_shown: usize,
    pub lines_in_source: usize,
    pub lines_omitted: bool,
    pub lines_shortened: bool,
}

pub fn prepare_edit_text(text: &str, max_lines: usize) -> PreparedText {
    let mut content = String::with_capacity(text.len().min(max_lines.saturating_mul(8)));
    let mut lines_in_source = 0usize;
    let mut lines_shown = 0usize;
    let mut lines_shortened = false;

    for line in text.lines() {
        lines_in_source += 1;
        if lines_shown >= max_lines {
            continue;
        }
        if lines_shown > 0 {
            content.push_str("\r\n");
        }
        if push_capped_line(&mut content, line) {
            lines_shortened = true;
        }
        lines_shown += 1;
    }

    PreparedText {
        content,
        lines_shown,
        lines_in_source,
        lines_omitted: lines_in_source > lines_shown,
        lines_shortened,
    }
}

/// Append up to [`MAX_LINE_CHARS`] scalar values. Returns true when the line was cut.
fn push_capped_line(out: &mut String, line: &str) -> bool {
    let mut chars = line.chars();
    let mut written = 0usize;
    while written < MAX_LINE_CHARS {
        match chars.next() {
            Some(ch) => {
                out.push(ch);
                written += 1;
            }
            None => return false,
        }
    }
    if chars.next().is_some() {
        out.push('\u{2026}');
        true
    } else {
        false
    }
}

/// Text sniff. A partial UTF-8 sequence is allowed only at the end of the
/// inspected prefix when the file continues past it (the 8 KB sniff edge, or a
/// byte-budget cut). The same partial sequence at EOF is not text.
fn passes_text_sniff(buffer: &[u8], total_bytes: u64) -> bool {
    let sniff_len = buffer.len().min(BINARY_SNIFF_BYTES);
    let sniff = &buffer[..sniff_len];
    if sniff.contains(&0) {
        return false;
    }
    match std::str::from_utf8(sniff) {
        Ok(_) => true,
        Err(err) => err.error_len().is_none() && (sniff_len as u64) < total_bytes,
    }
}

fn decode_utf8_prefix(bytes: &[u8], cap_cut: bool) -> (String, u64) {
    let bom = if bytes.starts_with(b"\xEF\xBB\xBF") {
        3
    } else {
        0
    };
    let mut body = &bytes[bom..];
    if cap_cut {
        body = trim_incomplete_utf8(body);
    }
    let text = match std::str::from_utf8(body) {
        Ok(text) => text.to_string(),
        Err(_) => String::from_utf8_lossy(body).into_owned(),
    };
    (text, (bom + body.len()) as u64)
}

/// Drop a trailing incomplete UTF-8 sequence so a budget cut does not become U+FFFD.
fn trim_incomplete_utf8(bytes: &[u8]) -> &[u8] {
    if bytes.is_empty() {
        return bytes;
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes,
        Err(err) if err.error_len().is_none() => &bytes[..err.valid_up_to()],
        Err(_) => match trailing_incomplete_len(bytes) {
            Some(n) if n > 0 && n <= bytes.len() => &bytes[..bytes.len() - n],
            _ => bytes,
        },
    }
}

fn trailing_incomplete_len(bytes: &[u8]) -> Option<usize> {
    let len = bytes.len();
    if len == 0 {
        return None;
    }
    let mut i = len - 1;
    let mut walked = 0usize;
    while walked < 3 && bytes[i] & 0b1100_0000 == 0b1000_0000 {
        walked += 1;
        if i == 0 {
            return None;
        }
        i -= 1;
    }
    if bytes[i] & 0b1100_0000 == 0b1000_0000 {
        return None;
    }
    let width = match bytes[i] {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => return None,
    };
    let have = len - i;
    if have < width {
        Some(have)
    } else {
        None
    }
}

fn decode_utf16_prefix(
    payload: &[u8],
    little_endian: bool,
    cap_cut: bool,
    bom_len: u64,
) -> (String, u64) {
    let mut end = payload.len() - (payload.len() % 2);
    let mut units = Vec::with_capacity(end / 2);
    for pair in payload[..end].chunks_exact(2) {
        let unit = if little_endian {
            u16::from_le_bytes([pair[0], pair[1]])
        } else {
            u16::from_be_bytes([pair[0], pair[1]])
        };
        units.push(unit);
    }
    // A budget cut can leave the high half of a surrogate pair. Drop it so the
    // preview does not end with U+FFFD that our read introduced. A file that
    // itself ends on a lone surrogate still goes through the lossy path.
    if cap_cut {
        if let Some(&last) = units.last() {
            if (0xD800..=0xDBFF).contains(&last) {
                units.pop();
                end = end.saturating_sub(2);
            }
        }
    }
    let text = String::from_utf16_lossy(&units);
    (text, bom_len + end as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn write_file(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.path().join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    fn encode_utf16(s: &str, little_endian: bool) -> Vec<u8> {
        let mut out = if little_endian {
            vec![0xFF, 0xFE]
        } else {
            vec![0xFE, 0xFF]
        };
        for unit in s.encode_utf16() {
            if little_endian {
                out.extend_from_slice(&unit.to_le_bytes());
            } else {
                out.extend_from_slice(&unit.to_be_bytes());
            }
        }
        out
    }

    fn live_token() -> (CancelToken, Generation) {
        let token = CancelToken::new();
        let gen = token.generate();
        (token, gen)
    }

    fn load(path: &Path, limits: PreviewLimits) -> Result<Preview, PreviewError> {
        let (token, gen) = live_token();
        load_text_preview(path, limits, &token, gen)
    }

    /// 262,143 ASCII `a`s then 100 × `é`, the #31 fixture.
    fn utf8_boundary_fixture() -> Vec<u8> {
        let mut data = vec![b'a'; 262_143];
        for _ in 0..100 {
            data.extend_from_slice("é".as_bytes());
        }
        data
    }

    #[test]
    fn default_budget_is_64_kib() {
        let limits = PreviewLimits::default();
        assert_eq!(limits.max_bytes, 64 * 1024);
        assert_eq!(limits.max_lines, 2_000);
        assert_eq!(MAX_LINE_CHARS, 2_000);
    }

    #[test]
    fn bounded_read_truncates_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let big = vec![b'a'; 300];
        let p = write_file(&dir, "trunc.txt", &big);
        let preview = load(&p, PreviewLimits::new(64, 10)).unwrap();
        assert_eq!(preview.content.len(), 64);
        assert!(preview.truncated);
        assert_eq!(preview.line_count, 1);
        let info = preview.truncation.as_ref().unwrap();
        assert_eq!(info.bytes_shown, 64);
        assert_eq!(info.total_bytes, 300);
        assert!(!preview.content.contains('\u{FFFD}'));
        assert_eq!(
            preview_text(&preview),
            format!(
                "Showing the first 64 bytes of 300 bytes\r\n\r\n{}",
                preview.content
            )
        );
        assert!(!preview_text(&preview).contains("[preview truncated]"));
    }

    #[test]
    fn file_that_fits_the_budget_is_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let data = vec![b'a'; 128];
        let p = write_file(&dir, "exact.txt", &data);
        let preview = load(&p, PreviewLimits::new(128, 10)).unwrap();
        assert!(!preview.truncated);
        assert!(preview.truncation.is_none());
        assert_eq!(preview.content, "a".repeat(128));
        assert_eq!(preview_text(&preview), preview.content);
    }

    #[test]
    fn binary_file_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "bin.txt", b"ok\x00binary");
        assert_eq!(
            load(&p, PreviewLimits::default()),
            Err(PreviewError::Binary)
        );
    }

    #[test]
    fn nul_byte_rejects_extensionless_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "Makefile", b"hello\0world");
        assert_eq!(
            load(&p, PreviewLimits::default()),
            Err(PreviewError::Binary)
        );
    }

    #[test]
    fn short_invalid_utf8_is_binary() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "lossy.txt", b"hello \xFF world");
        assert_eq!(
            load(&p, PreviewLimits::default()),
            Err(PreviewError::Binary)
        );
        let partial = write_file(&dir, "partial.txt", b"hello \xC3");
        assert_eq!(
            load(&partial, PreviewLimits::default()),
            Err(PreviewError::Binary)
        );
    }

    #[test]
    fn invalid_utf8_after_sniff_window_is_lossy() {
        let dir = tempfile::tempdir().unwrap();
        // Lines are long enough that the bad byte sits past 8 KB and still
        // inside the 2,000-line cap, so the replacement character stays visible.
        let mut data = Vec::new();
        while data.len() < BINARY_SNIFF_BYTES {
            data.extend_from_slice(b"abcd\n");
        }
        data.extend_from_slice(b"bad \xFF here\n");
        let p = write_file(&dir, "later.txt", &data);
        let preview = load(&p, PreviewLimits::default()).unwrap();
        assert!(preview.content.contains('\u{FFFD}'));
    }

    #[test]
    fn sniff_rules() {
        assert!(passes_text_sniff(b"hello", 5));
        assert!(passes_text_sniff(b"", 0));
        assert!(!passes_text_sniff(b"abc\0def", 7));
        assert!(!passes_text_sniff(b"hello \xFF world", 13));
        // Incomplete sequence at EOF is not text.
        assert!(!passes_text_sniff(b"hello \xC3", 7));
        // The same prefix is text when the file continues past the sniff slice.
        assert!(passes_text_sniff(b"hello \xC3", 9));

        let mut window = vec![b'a'; BINARY_SNIFF_BYTES - 1];
        window.push(0xC3);
        assert_eq!(window.len(), BINARY_SNIFF_BYTES);
        assert!(passes_text_sniff(&window, (BINARY_SNIFF_BYTES as u64) + 1));
        assert!(!passes_text_sniff(&window, window.len() as u64));
    }

    #[test]
    fn partial_utf8_at_sniff_boundary_is_text() {
        let line = b"abcd\n";
        let mut data = Vec::new();
        for _ in 0..(8190 / line.len()) {
            data.extend_from_slice(line);
        }
        assert_eq!(data.len(), 8190);
        data.push(b'z');
        data.push(0xC3);
        assert_eq!(data.len(), BINARY_SNIFF_BYTES);
        data.push(0xA9);
        data.extend_from_slice(b"\nend\n");

        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "sniff-boundary", &data);
        let preview = load(&p, PreviewLimits::default()).unwrap();
        assert!(preview.content.contains('é'), "{}", preview.content);
        assert!(preview.content.contains("end"));
        assert!(!preview.content.contains('\u{FFFD}'));
        assert!(!preview.truncated);
    }

    #[test]
    fn extensionless_and_dotfiles_are_text_when_content_is() {
        let dir = tempfile::tempdir().unwrap();
        let names = [
            "Makefile",
            "Dockerfile",
            "LICENSE",
            "README",
            ".gitignore",
            ".editorconfig",
            "script.go",
            "Main.java",
            "query.sql",
            "comp.tsx",
            "app.config",
        ];
        for name in names {
            let p = write_file(&dir, name, b"hello\nworld");
            let preview = load(&p, PreviewLimits::default()).unwrap();
            assert_eq!(preview.content, "hello\r\nworld", "{name}");
            assert!(!preview.truncated, "{name}");
        }
    }

    #[test]
    fn utf16_extensionless_file_is_text() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "Makefile", &encode_utf16("all:\n\ttrue\n", true));
        let preview = load(&p, PreviewLimits::default()).unwrap();
        assert_eq!(preview.content, "all:\r\n\ttrue");
        assert!(!preview.truncated);
    }

    #[test]
    fn denied_extensions_use_their_own_kind() {
        let dir = tempfile::tempdir().unwrap();
        let png = write_file(&dir, "pic.PNG", b"not really a png\x00");
        assert_eq!(
            load(&png, PreviewLimits::default()),
            Err(PreviewError::Denied(DeniedKind::Image))
        );
        let jpg = write_file(&dir, "photo.JpEg", b"hello");
        assert_eq!(
            load(&jpg, PreviewLimits::default()),
            Err(PreviewError::Denied(DeniedKind::Image))
        );
        for name in [
            "app.EXE",
            "lib.Dll",
            "notes.docx",
            "sheet.xlsx",
            "deck.pptx",
            "book.pdf",
            "song.mp3",
            "clip.mp4",
            "data.zip",
            "pack.7z",
            "old.rar",
            "disk.iso",
            "setup.msi",
        ] {
            let p = write_file(&dir, name, b"hello");
            assert_eq!(
                load(&p, PreviewLimits::default()),
                Err(PreviewError::Denied(DeniedKind::Binary)),
                "{name}"
            );
        }
    }

    #[test]
    fn denied_kind_matches_case_insensitively() {
        assert_eq!(denied_kind(Path::new("A.PnG")), Some(DeniedKind::Image));
        assert_eq!(
            denied_kind(Path::new(r"C:\temp\app.EXE")),
            Some(DeniedKind::Binary)
        );
        assert_eq!(denied_kind(Path::new("Makefile")), None);
        assert_eq!(denied_kind(Path::new(".gitignore")), None);
        assert_eq!(denied_kind(Path::new("README")), None);
        assert_eq!(denied_kind(Path::new("notes.TXT")), None);
        assert_eq!(denied_kind(Path::new("picture.svg")), None);
    }

    #[test]
    fn case_insensitive_extensions_still_load() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = write_file(&dir, "README.TXT", b"readme content");
        let p2 = write_file(&dir, "notes.Md", b"notes content");
        let p3 = write_file(&dir, "SETUP.LOG", b"setup log");
        assert_eq!(
            load(&p1, PreviewLimits::default()).unwrap().content,
            "readme content"
        );
        assert_eq!(
            load(&p2, PreviewLimits::default()).unwrap().content,
            "notes content"
        );
        assert_eq!(
            load(&p3, PreviewLimits::default()).unwrap().content,
            "setup log"
        );
    }

    #[test]
    fn bom_is_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "bom.txt", b"\xEF\xBB\xBFhello world");
        let preview = load(&p, PreviewLimits::default()).unwrap();
        assert_eq!(preview.content, "hello world");
        assert!(!preview.truncated);
    }

    #[test]
    fn utf16le_bom_is_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let text = "hello world é\nsecond line";
        let p = write_file(&dir, "utf16le.txt", &encode_utf16(text, true));
        let preview = load(&p, PreviewLimits::default()).unwrap();
        assert_eq!(preview.content, "hello world é\r\nsecond line");
        assert!(!preview.truncated);
        assert_eq!(preview.line_count, 2);
    }

    #[test]
    fn utf16be_bom_is_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let text = "hello world é";
        let p = write_file(&dir, "utf16be.txt", &encode_utf16(text, false));
        let preview = load(&p, PreviewLimits::default()).unwrap();
        assert_eq!(preview.content, text);
        assert!(!preview.truncated);
    }

    #[test]
    fn utf16_odd_byte_cap_is_truncated_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let encoded = encode_utf16("hello world", true);
        let p = write_file(&dir, "utf16odd.txt", &encoded);
        // BOM (2) + 'h' (2) + first byte of 'e' = 5, so the last code unit is cut.
        let preview = load(&p, PreviewLimits::new(5, 10)).unwrap();
        assert!(preview.truncated);
        assert_eq!(preview.content, "h");
        assert!(!preview.content.contains('\u{FFFD}'));
        let info = preview.truncation.as_ref().unwrap();
        assert_eq!(info.bytes_shown, 4);
        assert_eq!(info.total_bytes, encoded.len() as u64);
    }

    #[test]
    fn utf16_surrogate_pair_roundtrips() {
        let text = "A\u{1F600}B";
        let dir = tempfile::tempdir().unwrap();
        for little in [true, false] {
            let name = if little {
                "emoji-le.txt"
            } else {
                "emoji-be.txt"
            };
            let p = write_file(&dir, name, &encode_utf16(text, little));
            let preview = load(&p, PreviewLimits::default()).unwrap();
            assert_eq!(preview.content, text, "le={little}");
            assert!(!preview.truncated, "le={little}");
            assert!(!preview.content.contains('\u{FFFD}'));
        }
    }

    #[test]
    fn utf16_surrogate_cut_drops_dangling_high_surrogate() {
        let text = "A\u{1F600}B";
        let dir = tempfile::tempdir().unwrap();
        for little in [true, false] {
            let encoded = encode_utf16(text, little);
            let name = if little { "cut-le.txt" } else { "cut-be.txt" };
            let p = write_file(&dir, name, &encoded);
            // BOM + 'A' + high surrogate, and one extra byte into the low surrogate.
            for budget in [6usize, 7] {
                let preview = load(&p, PreviewLimits::new(budget, 10)).unwrap();
                assert_eq!(preview.content, "A", "le={little} budget={budget}");
                assert!(!preview.content.contains('\u{FFFD}'));
                let info = preview.truncation.as_ref().unwrap();
                assert_eq!(info.bytes_shown, 4, "le={little} budget={budget}");
                assert_eq!(info.total_bytes, encoded.len() as u64);
            }
            // Budget that includes the whole pair but not the final 'B'.
            let preview = load(&p, PreviewLimits::new(8, 10)).unwrap();
            assert_eq!(preview.content, "A\u{1F600}", "le={little}");
            assert!(!preview.content.contains('\u{FFFD}'));
            assert_eq!(preview.truncation.as_ref().unwrap().bytes_shown, 8);
        }
    }

    #[test]
    fn utf8_budget_cut_does_not_split_char() {
        let data = utf8_boundary_fixture();
        assert_eq!(data[256 * 1024 - 1], 0xC3);
        let (text, shown) = decode_utf8_prefix(&data[..256 * 1024], true);
        assert_eq!(shown, 262_143);
        assert_eq!(text.len(), 262_143);
        assert!(text.bytes().all(|b| b == b'a'));
        assert!(!text.contains('\u{FFFD}'));

        let (text, shown) = decode_utf8_prefix(&data[..64 * 1024], true);
        assert_eq!(shown, 64 * 1024);
        assert_eq!(text.len(), 64 * 1024);
        assert!(!text.contains('\u{FFFD}'));

        // The new budget also backs off when the cut lands inside `é`.
        let mut at_budget = vec![b'a'; 64 * 1024 - 1];
        at_budget.extend_from_slice("é".as_bytes());
        assert_eq!(at_budget[64 * 1024 - 1], 0xC3);
        let (text, shown) = decode_utf8_prefix(&at_budget[..64 * 1024], true);
        assert_eq!(shown, 64 * 1024 - 1);
        assert!(text.bytes().all(|b| b == b'a'));
        assert!(!text.contains('\u{FFFD}'));
    }

    #[test]
    fn utf8_boundary_file_at_both_budgets() {
        let data = utf8_boundary_fixture();
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "utf8-boundary.txt", &data);
        let cases = [
            (256 * 1024, 262_143u64, "256 KB"),
            (64 * 1024, 64 * 1024, "64 KB"),
        ];
        for (budget, shown, shown_label) in cases {
            let preview = load(&p, PreviewLimits::new(budget, 10_000)).unwrap();
            assert!(preview.truncated, "budget {budget}");
            assert!(!preview.content.contains('\u{FFFD}'), "budget {budget}");
            assert!(preview.content.ends_with('\u{2026}'), "budget {budget}");
            assert!(preview.content.chars().take(2000).all(|c| c == 'a'));
            let info = preview.truncation.as_ref().unwrap();
            assert_eq!(info.bytes_shown, shown, "budget {budget}");
            assert_eq!(info.total_bytes, data.len() as u64);
            assert!(info.lines_shortened);
            let notice = format_truncation_notice(info).unwrap();
            assert!(
                notice.starts_with(&format!("Showing the first {shown_label} of ")),
                "{notice}"
            );
            assert!(notice.contains("Some lines were shortened."));
            let body = preview_text(&preview);
            assert!(body.starts_with(&notice));
            assert!(!body.contains("[preview truncated]"));
        }
    }

    #[test]
    fn load_utf8_cut_is_visible_inside_the_line_cap() {
        let mut data = vec![b'a'; 10];
        data.extend_from_slice("é".as_bytes());
        data.extend_from_slice("é".as_bytes());
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "small-cut.txt", &data);
        let preview = load(&p, PreviewLimits::new(11, 10)).unwrap();
        assert_eq!(preview.content, "a".repeat(10));
        assert!(!preview.content.contains('\u{FFFD}'));
        assert!(!preview.content.contains('é'));
        let info = preview.truncation.as_ref().unwrap();
        assert_eq!(info.bytes_shown, 10);
        assert_eq!(info.total_bytes, data.len() as u64);
        assert!(!info.lines_shortened);
    }

    #[test]
    fn line_cap_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mut data = String::new();
        for i in 0..3000 {
            data.push_str(&format!("line{i}\n"));
        }
        let p = write_file(&dir, "lines.txt", data.as_bytes());
        let preview = load(&p, PreviewLimits::default()).unwrap();
        assert_eq!(preview.line_count, 2000);
        assert!(preview.truncated);
        assert!(preview.content.starts_with("line0\r\nline1\r\n"));
        assert!(preview.content.contains("line1999"));
        assert!(!preview.content.contains("line2000"));
        let info = preview.truncation.as_ref().unwrap();
        assert_eq!(info.lines_shown, 2000);
        assert_eq!(info.total_lines, Some(3000));
        assert!(info.lines_omitted);
        assert!(!info.lines_shortened);
        assert_eq!(
            format_truncation_notice(info).as_deref(),
            Some("Showing the first 2,000 of 3,000 lines")
        );
        assert!(
            preview_text(&preview).starts_with("Showing the first 2,000 of 3,000 lines\r\n\r\n")
        );
    }

    #[test]
    fn long_lines_are_capped_on_char_boundary() {
        let mut line = "a".repeat(1999);
        line.push('é');
        line.push('b');
        line.push('é');
        let prepared = prepare_edit_text(&line, 10);
        let expected = format!("{}é…", "a".repeat(1999));
        assert_eq!(prepared.content, expected);
        assert!(prepared.lines_shortened);
        assert_eq!(prepared.content.chars().count(), MAX_LINE_CHARS + 1);
        assert!(!prepared.content.contains('\u{FFFD}'));
        assert!(!prepared.content.contains('b'));

        let exact = "é".repeat(MAX_LINE_CHARS);
        let prepared = prepare_edit_text(&exact, 10);
        assert_eq!(prepared.content, exact);
        assert!(!prepared.lines_shortened);

        let emoji = "\u{1F600}".repeat(MAX_LINE_CHARS + 1);
        let prepared = prepare_edit_text(&emoji, 10);
        assert_eq!(
            prepared
                .content
                .chars()
                .filter(|c| *c == '\u{1F600}')
                .count(),
            MAX_LINE_CHARS
        );
        assert!(prepared.content.ends_with('\u{2026}'));
        assert!(!prepared.content.contains('\u{FFFD}'));
        assert_eq!(prepared.content.chars().count(), MAX_LINE_CHARS + 1);

        let mixed = format!("short\r\n{line}\n");
        let prepared = prepare_edit_text(&mixed, 10);
        assert_eq!(prepared.content, format!("short\r\n{expected}"));
        assert!(prepared.lines_shortened);
        assert_eq!(prepared.lines_shown, 2);
    }

    #[test]
    fn prepare_normalises_line_endings_to_crlf() {
        // `\n` and `\r\n` become CRLF. A lone `\r` stays in the line, matching
        // the previous EDIT-control conversion.
        let prepared = prepare_edit_text("a\nb\r\nc\rd", 10);
        assert_eq!(prepared.content, "a\r\nb\r\nc\rd");
        assert!(!prepared.lines_shortened);
        assert_eq!(prepared.lines_shown, 3);
        let already = prepare_edit_text("a\r\nb\r\nc", 10);
        assert_eq!(already.content, "a\r\nb\r\nc");

        let capped = prepare_edit_text("a\nb\nc\n", 2);
        assert_eq!(capped.content, "a\r\nb");
        assert!(capped.lines_omitted);
        assert_eq!(capped.lines_in_source, 3);
        assert_eq!(capped.lines_shown, 2);
    }

    #[test]
    fn load_records_shortened_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "long.txt", "a".repeat(2001).as_bytes());
        let preview = load(&p, PreviewLimits::default()).unwrap();
        assert!(preview.truncated);
        assert_eq!(preview.content.chars().count(), MAX_LINE_CHARS + 1);
        assert!(preview.content.ends_with('\u{2026}'));
        assert!(!preview.content.contains('\u{FFFD}'));
        let info = preview.truncation.as_ref().unwrap();
        assert!(info.lines_shortened);
        assert_eq!(info.bytes_shown, info.total_bytes);
        assert_eq!(
            format_truncation_notice(info).as_deref(),
            Some("Some lines were shortened.")
        );
        assert_eq!(
            preview_text(&preview),
            format!("Some lines were shortened.\r\n\r\n{}", preview.content)
        );
    }

    #[test]
    fn truncation_notice_formats_sizes_and_lines() {
        assert_eq!(format_byte_size(0), "0 bytes");
        assert_eq!(format_byte_size(1), "1 byte");
        assert_eq!(format_byte_size(512), "512 bytes");
        assert_eq!(format_byte_size(1023), "1023 bytes");
        assert_eq!(format_byte_size(1024), "1 KB");
        assert_eq!(format_byte_size(1536), "1.5 KB");
        assert_eq!(format_byte_size(64 * 1024), "64 KB");
        assert_eq!(format_byte_size(256 * 1024), "256 KB");
        assert_eq!(format_byte_size(262_143), "256 KB");
        assert_eq!(format_byte_size(1024 * 1024), "1 MB");
        assert_eq!(format_byte_size(1_258_291), "1.2 MB");
        assert_eq!(format_byte_size(1024 * 1024 * 1024), "1 GB");

        let bytes_only = Truncation {
            bytes_shown: 64 * 1024,
            total_bytes: 1_258_291,
            lines_shown: 10,
            total_lines: None,
            lines_omitted: false,
            lines_shortened: false,
        };
        assert_eq!(
            format_truncation_notice(&bytes_only).as_deref(),
            Some("Showing the first 64 KB of 1.2 MB")
        );

        let lines_only = Truncation {
            bytes_shown: 100,
            total_bytes: 100,
            lines_shown: 2_000,
            total_lines: Some(48_113),
            lines_omitted: true,
            lines_shortened: false,
        };
        assert_eq!(
            format_truncation_notice(&lines_only).as_deref(),
            Some("Showing the first 2,000 of 48,113 lines")
        );

        let both = Truncation {
            bytes_shown: 256 * 1024,
            total_bytes: 5 * 1024 * 1024,
            lines_shown: 2_000,
            total_lines: None,
            lines_omitted: true,
            lines_shortened: true,
        };
        assert_eq!(
            format_truncation_notice(&both).as_deref(),
            Some("Showing the first 256 KB of 5 MB (2,000 lines). Some lines were shortened.")
        );

        let untouched = Truncation {
            bytes_shown: 10,
            total_bytes: 10,
            lines_shown: 1,
            total_lines: Some(1),
            lines_omitted: false,
            lines_shortened: false,
        };
        assert_eq!(format_truncation_notice(&untouched), None);
    }

    #[test]
    fn stale_generation_returns_cancelled() {
        let token = CancelToken::new();
        let stale = token.generate();
        let _live = token.generate();
        assert!(token.is_cancelled(&stale));

        let dir = tempfile::tempdir().unwrap();
        let p = write_file(&dir, "cancelled.txt", b"hello");
        assert_eq!(
            load_text_preview(&p, PreviewLimits::default(), &token, stale),
            Err(PreviewError::Cancelled)
        );
    }

    #[test]
    fn directory_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let sub_dir = dir.path().join("spacepeek_test_dir");
        std::fs::create_dir_all(&sub_dir).unwrap();
        assert_eq!(
            load(&sub_dir, PreviewLimits::default()),
            Err(PreviewError::Directory {
                count: Some(DirectoryCount::Exact(0))
            })
        );
    }

    #[test]
    fn directory_message_names_the_folder_and_caps_the_count() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("写真");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("a.txt"), b"a").unwrap();
        fs::write(folder.join("b.txt"), b"b").unwrap();
        let err = load(&folder, PreviewLimits::default()).unwrap_err();
        assert_eq!(
            err,
            PreviewError::Directory {
                count: Some(DirectoryCount::Exact(2))
            }
        );
        assert_eq!(err.user_message("写真"), "Folder: 写真 · 2 items");
        assert_eq!(err.header_status(), "2 items");
        assert_eq!(err.to_string(), "is a directory");

        let counted = dir.path().join("counted");
        fs::create_dir(&counted).unwrap();
        for i in 0..4 {
            fs::write(counted.join(format!("f{i}")), b"x").unwrap();
        }
        assert_eq!(
            directory_count_capped(&counted, 3),
            Some(DirectoryCount::Capped)
        );
        assert_eq!(
            directory_count_capped(&counted, 4),
            Some(DirectoryCount::Exact(4))
        );
        assert_eq!(
            directory_count_capped(&counted, 10),
            Some(DirectoryCount::Exact(4))
        );
        assert_eq!(
            directory_count_capped(&folder, 2),
            Some(DirectoryCount::Exact(2))
        );
        assert_eq!(
            directory_count_capped(&folder, 1),
            Some(DirectoryCount::Capped)
        );

        assert_eq!(
            format_directory_message("photos", None),
            "Folder: photos"
        );
        assert_eq!(
            format_directory_message("photos", Some(DirectoryCount::Exact(0))),
            "Folder: photos · 0 items"
        );
        assert_eq!(
            format_directory_message("photos", Some(DirectoryCount::Exact(1))),
            "Folder: photos · 1 item"
        );
        assert_eq!(
            format_directory_message("photos", Some(DirectoryCount::Exact(12))),
            "Folder: photos · 12 items"
        );
        assert_eq!(
            format_directory_message("photos", Some(DirectoryCount::Exact(10_000))),
            "Folder: photos · 10,000 items"
        );
        assert_eq!(
            format_directory_message("photos", Some(DirectoryCount::Capped)),
            "Folder: photos · 10,000+ items"
        );
        assert_eq!(DIRECTORY_COUNT_CAP, 10_000);
        assert_eq!(format_directory_message("", None), "Folder: This file");
    }

    #[test]
    fn empty_file_is_distinct_from_a_blank_preview() {
        let dir = tempfile::tempdir().unwrap();
        let empty = write_file(&dir, "empty.txt", b"");
        let err = load(&empty, PreviewLimits::default()).unwrap_err();
        assert_eq!(err, PreviewError::Empty);
        assert_eq!(err.user_message("empty.txt"), "This file is empty");
        assert_eq!(err.header_status(), "0 B");
        assert_eq!(err.to_string(), "empty file");
        assert_ne!(err.to_string(), err.user_message("empty.txt"));

        let png = write_file(&dir, "photo.png", b"");
        assert_eq!(
            load(&png, PreviewLimits::default()),
            Err(PreviewError::Denied(DeniedKind::Image))
        );
    }

    #[test]
    fn shortcut_is_not_opened_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let link = write_file(&dir, "Notes.LNK", b"not really a shortcut");
        assert_eq!(
            load(&link, PreviewLimits::default()),
            Err(PreviewError::Shortcut { target: None })
        );
        let err = PreviewError::Shortcut { target: None };
        assert_eq!(
            err.user_message("Notes.LNK"),
            "Shortcut — previewing shortcuts isn't supported yet"
        );
        assert_eq!(err.header_status(), "Shortcut");
        assert_eq!(err.to_string(), "shortcut");
        let with_target = PreviewError::Shortcut {
            target: Some(r"C:\docs\ノート.md".to_string()),
        };
        assert_eq!(
            with_target.user_message("Notes.LNK"),
            "Shortcut to C:\\docs\\ノート.md"
        );
        let blank = PreviewError::Shortcut {
            target: Some("  ".to_string()),
        };
        assert_eq!(
            blank.user_message("Notes.LNK"),
            "Shortcut — previewing shortcuts isn't supported yet"
        );
    }

    #[test]
    fn user_message_covers_each_error_kind() {
        assert_eq!(
            PreviewError::Binary.user_message("notes.md"),
            "notes.md doesn't look like text"
        );
        assert_eq!(
            PreviewError::Binary.user_message("ノート.md"),
            "ノート.md doesn't look like text"
        );
        assert_eq!(PreviewError::Binary.to_string(), "file looks binary");
        assert_eq!(PreviewError::Binary.header_status(), "Not text");

        let image = PreviewError::Denied(DeniedKind::Image);
        assert_eq!(
            image.user_message("photo.png"),
            "photo.png can't be previewed — this file type isn't supported yet"
        );
        assert_eq!(
            image.user_message("café.png"),
            "café.png can't be previewed — this file type isn't supported yet"
        );
        assert_eq!(image.to_string(), "image file");
        assert_eq!(image.header_status(), "Not supported");

        let binary = PreviewError::Denied(DeniedKind::Binary);
        assert_eq!(
            binary.user_message("archive.zip"),
            "archive.zip can't be previewed — this file type isn't supported yet"
        );
        assert_eq!(binary.to_string(), "unsupported file type");

        let missing = PreviewError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "The system cannot find the file specified. (os error 2)",
        ));
        assert_eq!(missing.user_message("notes.md"), "notes.md no longer exists");
        assert_eq!(
            missing.user_message("ノート.md"),
            "ノート.md no longer exists"
        );
        assert!(missing.to_string().contains("io error"));
        assert!(!missing.user_message("notes.md").contains("os error"));
        assert!(!missing.user_message("notes.md").contains("io error"));
        assert_eq!(missing.header_status(), "Missing");

        let raw_missing = PreviewError::Io(std::io::Error::from_raw_os_error(2));
        assert_eq!(
            raw_missing.user_message("notes.md"),
            "notes.md no longer exists"
        );
        let raw_path = PreviewError::Io(std::io::Error::from_raw_os_error(3));
        assert_eq!(
            raw_path.user_message("notes.md"),
            "notes.md no longer exists"
        );

        let sharing = PreviewError::Io(std::io::Error::from_raw_os_error(32));
        assert_eq!(
            sharing.user_message("notes.md"),
            "notes.md is open in another program and can't be read"
        );
        assert_eq!(
            sharing.user_message("café.txt"),
            "café.txt is open in another program and can't be read"
        );
        assert!(sharing.to_string().contains("io error"));
        assert!(!sharing.user_message("notes.md").contains("os error"));
        assert!(!sharing.user_message("notes.md").contains("32"));
        assert_eq!(sharing.header_status(), "In use");

        let locked = PreviewError::Io(std::io::Error::from_raw_os_error(33));
        assert_eq!(
            locked.user_message("notes.md"),
            "notes.md is open in another program and can't be read"
        );

        let denied = PreviewError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Access is denied. (os error 5)",
        ));
        assert_eq!(
            denied.user_message("notes.md"),
            "Access to notes.md was denied"
        );
        assert_eq!(
            denied.user_message("ノート.md"),
            "Access to ノート.md was denied"
        );
        assert!(!denied.user_message("notes.md").contains("os error"));
        assert_eq!(denied.header_status(), "Access denied");

        let raw_denied = PreviewError::Io(std::io::Error::from_raw_os_error(5));
        assert_eq!(
            raw_denied.user_message("secret.txt"),
            "Access to secret.txt was denied"
        );

        let other = PreviewError::Io(std::io::Error::other("could not start preview load"));
        assert_eq!(other.user_message("notes.md"), "notes.md can't be read");
        assert_eq!(other.user_message(""), "This file can't be read");
        assert!(!other.user_message("notes.md").contains("could not start"));
        assert_eq!(other.header_status(), "Can't read");

        assert_eq!(
            PreviewError::Cancelled.user_message("notes.md"),
            "Preview cancelled"
        );
        assert_eq!(format_compact_size(0), "0 B");
        assert_eq!(format_compact_size(1), "1 B");
        assert_eq!(format_compact_size(512), "512 B");
        assert_eq!(format_compact_size(12 * 1024), "12 KB");
        assert_eq!(format_compact_size(1536), "1.5 KB");
        assert_eq!(format_compact_size(1024 * 1024), "1 MB");
        assert_eq!(format_compact_size(1_258_291), "1.2 MB");
        assert_eq!(format_compact_size(1024 * 1024 * 1024), "1 GB");
    }
}
