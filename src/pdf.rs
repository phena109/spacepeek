//! First-page PDF preview that does not touch Win32.
//!
//! Detection, caps, fit math, failure wording, the icon-then-page session,
//! and the worker's cancel/timeout decision live here so they run on Linux.
//! Windows renders the page with `Windows.Data.Pdf` on a dedicated worker
//! and only after a file has already been classified as a PDF. Text, raster,
//! Markdown, and SVG loads do not call into this module's render path.
//!
//! The page bitmap is produced once, at the window body size and the monitor
//! DPI. Zoom scales that bitmap. The WinRT document is not kept after the
//! pixels and the page count have been copied.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use crate::geometry::effective_dpi;
use crate::image_preview::{fit_size, DecodedImage, ImageFormat};
use crate::preview::{format_compact_size, format_grouped, Generation, Preview};

/// Safety cap on the PDF file before it is handed to the Windows PDF engine.
///
/// Not from a measured failure. The Windows ARM64 spike rendered a 320-page
/// text book and a large scan; this limit only refuses an unbounded file.
/// 128 MiB leaves room for a scan wrapped in a PDF, above the 64 MiB raster
/// file cap.
pub const PDF_MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;
/// Output pixel cap. Also kept within the image path's pixel cap (100 MP)
/// by [`PdfLimits::default`]. A huge page is clamped; it is never allocated
/// at its raw size.
pub const PDF_MAX_PIXELS: u64 = 32_000_000;
/// Open plus first-page render. Matches the SVG parse budget.
pub const PDF_TIMEOUT: Duration = Duration::from_millis(1_500);
/// Head and tail read when a load fails, to look for a trailer `/Encrypt`.
/// Not a parse, and not used on the success path.
pub const PDF_ENCRYPT_SCAN_WINDOW: u64 = 1024 * 1024;

/// Caps for one PDF preview. The render target is the window body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfLimits {
    pub max_file_bytes: u64,
    pub max_pixels: u64,
    /// Longest rendered side, in pixels. Matches the image display box.
    pub max_dimension: u32,
    pub timeout: Duration,
}

impl Default for PdfLimits {
    fn default() -> Self {
        let image = crate::image_preview::ImageLimits::default();
        Self {
            max_file_bytes: PDF_MAX_FILE_BYTES,
            max_pixels: PDF_MAX_PIXELS.min(image.max_pixels),
            max_dimension: image.max_display_dimension.max(1),
            timeout: PDF_TIMEOUT,
        }
    }
}

/// Window body the first page is fitted into, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfTarget {
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
}

impl Default for PdfTarget {
    fn default() -> Self {
        Self {
            width: 1,
            height: 1,
            dpi: 96,
        }
    }
}

/// Why a PDF is showing the shell icon instead of a page, or why the
/// worker should stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfError {
    /// The icon is up and the page is still rendering. Not a failure.
    Rendering,
    FileTooLarge {
        bytes: u64,
    },
    PasswordProtected,
    Damaged,
    TimedOut,
    /// The fitted page would still exceed [`PDF_MAX_PIXELS`].
    TooManyPixels,
    InvalidSize,
    /// `RoInitialize` or WinRT activation failed. The icon stays.
    Unavailable,
    Cancelled,
    /// Win32 open/stat failure. `2`/`3` missing, `5` denied, `32`/`33` in use.
    /// Shown with the same sentence as any other file, not as a damaged PDF.
    Io(i32),
}

impl std::fmt::Display for PdfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PdfError::Rendering => write!(f, "pdf rendering"),
            PdfError::FileTooLarge { bytes } => write!(f, "pdf file too large ({bytes} bytes)"),
            PdfError::PasswordProtected => write!(f, "pdf password protected"),
            PdfError::Damaged => write!(f, "pdf damaged"),
            PdfError::TimedOut => write!(f, "pdf timed out"),
            PdfError::TooManyPixels => write!(f, "pdf page too many pixels"),
            PdfError::InvalidSize => write!(f, "pdf page has an invalid size"),
            PdfError::Unavailable => write!(f, "pdf preview unavailable"),
            PdfError::Cancelled => write!(f, "cancelled"),
            PdfError::Io(code) => write!(f, "pdf io error ({code})"),
        }
    }
}

impl PdfError {
    /// Sentence after `{name} can't be previewed —`. Empty while rendering:
    /// the window shows quick info and the shell icon, and the header says
    /// `PDF`.
    pub fn reason(&self) -> String {
        match self {
            PdfError::Rendering => String::new(),
            PdfError::FileTooLarge { bytes } => {
                format!("this PDF is too large ({})", format_compact_size(*bytes))
            }
            PdfError::PasswordProtected => "this PDF is password protected".to_string(),
            PdfError::Damaged => "this PDF looks damaged or isn't a valid PDF".to_string(),
            PdfError::TimedOut => "this PDF took too long to open".to_string(),
            PdfError::TooManyPixels => "this PDF page is too large to preview".to_string(),
            PdfError::InvalidSize => "this PDF page has an invalid size".to_string(),
            PdfError::Unavailable => "PDF preview isn't available".to_string(),
            PdfError::Cancelled => "preview cancelled".to_string(),
            PdfError::Io(_) => "this PDF can't be read".to_string(),
        }
    }

    pub fn user_message(&self, file_name: &str) -> String {
        let name = if file_name.is_empty() {
            "This file"
        } else {
            file_name
        };
        if let PdfError::Io(code) = self {
            return pdf_io_user_message(name, *code);
        }
        if matches!(self, PdfError::Rendering) {
            return String::new();
        }
        format!("{name} can't be previewed — {}", self.reason())
    }

    pub fn header_status(&self) -> String {
        match self {
            PdfError::Rendering => "PDF".to_string(),
            PdfError::FileTooLarge { .. } | PdfError::TooManyPixels => "Too large".to_string(),
            PdfError::PasswordProtected => "Password protected".to_string(),
            PdfError::Damaged => "Damaged".to_string(),
            PdfError::TimedOut => "Timed out".to_string(),
            PdfError::InvalidSize => "Invalid size".to_string(),
            PdfError::Unavailable => "Unavailable".to_string(),
            PdfError::Cancelled => "Cancelled".to_string(),
            PdfError::Io(code) => pdf_io_header(*code).to_string(),
        }
    }

    pub const fn is_placeholder(self) -> bool {
        matches!(self, PdfError::Rendering)
    }
}

/// What a stat of a `.pdf` (or a sniffed extensionless file) should do.
///
/// [`PdfGate::Render`] is the immediate icon. The page comes later, from the
/// worker. An empty file stays the shared empty-file message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfGate {
    Render,
    Empty,
    TooLarge { bytes: u64 },
}

pub fn gate_pdf_file(len: u64, limits: &PdfLimits) -> PdfGate {
    if len == 0 {
        PdfGate::Empty
    } else if len > limits.max_file_bytes {
        PdfGate::TooLarge { bytes: len }
    } else {
        PdfGate::Render
    }
}

/// True for `.pdf`, any case. `.pdfx` is not a PDF. A file whose name is
/// `.pdf` counts; `Path::extension` does not see that leading dot.
pub fn is_pdf_path(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        return ext.eq_ignore_ascii_case("pdf");
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(".pdf"))
}

/// True when `header` begins with `%PDF`. A UTF-8 BOM is skipped. This is a
/// prefix check, not a search through the file.
pub fn sniff_pdf(header: &[u8]) -> bool {
    let bytes = header.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(header);
    bytes.starts_with(b"%PDF")
}

/// Extensionless files only. A named `.txt`, `.png`, or `.svg` is never
/// sniffed, even when the bytes start with `%PDF`.
pub fn extensionless_pdf(path: &Path, header: &[u8]) -> bool {
    if is_pdf_path(path) || path.extension().is_some() {
        return false;
    }
    sniff_pdf(header)
}

/// Load failed. `password_protected` is `PdfDocument.IsPasswordProtected`
/// when the document opened. `encrypt_dict` is the `/Encrypt` probe used
/// when it did not. No password is tried.
pub fn classify_pdf_open_failure(password_protected: Option<bool>, encrypt_dict: bool) -> PdfError {
    if password_protected == Some(true) || encrypt_dict {
        PdfError::PasswordProtected
    } else {
        PdfError::Damaged
    }
}

/// `REGDB_E_CLASSNOTREG`. The PDF activation factory is not registered.
const HRESULT_CLASSNOTREG: i32 = 0x8004_0154_u32 as i32;
/// `CLASS_E_CLASSNOTAVAILABLE`.
const HRESULT_CLASSNOTAVAILABLE: i32 = 0x8004_0111_u32 as i32;
/// `CO_E_NOTINITIALIZED`. WinRT was used before `RoInitialize`.
const HRESULT_NOT_INITIALIZED: i32 = 0x8004_01F0_u32 as i32;

/// `true` when `hresult` means Windows.Data.Pdf could not be activated.
/// A damaged file uses some other code and stays a file error.
pub fn is_pdf_engine_unavailable(hresult: i32) -> bool {
    matches!(
        hresult,
        HRESULT_CLASSNOTREG | HRESULT_CLASSNOTAVAILABLE | HRESULT_NOT_INITIALIZED
    )
}

/// Activation failures become [`PdfError::Unavailable`]. Sharing, missing,
/// and access-denied Win32 codes become [`PdfError::Io`]. Anything else that
/// reaches this helper is a bad file, not a missing engine.
pub fn pdf_error_from_hresult(hresult: i32) -> PdfError {
    if is_pdf_engine_unavailable(hresult) {
        return PdfError::Unavailable;
    }
    if let Some(code) = win32_code_from_hresult(hresult) {
        if let Some(err) = pdf_error_from_win32(code) {
            return err;
        }
    }
    PdfError::Damaged
}

/// `HRESULT_FROM_WIN32`: facility 7, severity failure, code in the low word.
fn win32_code_from_hresult(hresult: i32) -> Option<i32> {
    let raw = hresult as u32;
    if raw & 0xFFFF_0000 != 0x8007_0000 {
        return None;
    }
    let code = (raw & 0xFFFF) as i32;
    if code == 0 {
        None
    } else {
        Some(code)
    }
}

/// Win32 codes the other preview paths already turn into a sentence.
/// `2` and `3` are missing, `5` is access denied, `32` and `33` are in use.
pub fn pdf_error_from_win32(code: i32) -> Option<PdfError> {
    match code {
        2 | 3 | 5 | 32 | 33 => Some(PdfError::Io(code)),
        _ => None,
    }
}

/// `metadata` / `File::open` failure on the worker. Raw Win32 codes win.
/// `NotFound` and `PermissionDenied` without a code use 2 and 5.
pub fn pdf_error_from_io(err: &std::io::Error) -> PdfError {
    if let Some(code) = err.raw_os_error() {
        if let Some(mapped) = pdf_error_from_win32(code) {
            return mapped;
        }
        return PdfError::Damaged;
    }
    match err.kind() {
        std::io::ErrorKind::NotFound => PdfError::Io(2),
        std::io::ErrorKind::PermissionDenied => PdfError::Io(5),
        _ => PdfError::Damaged,
    }
}

fn pdf_io_user_message(name: &str, code: i32) -> String {
    match code {
        32 | 33 => format!("{name} is open in another program and can't be read"),
        2 | 3 => format!("{name} no longer exists"),
        5 => format!("Access to {name} was denied"),
        _ => format!("{name} can't be read"),
    }
}

fn pdf_io_header(code: i32) -> &'static str {
    match code {
        32 | 33 => "In use",
        2 | 3 => "Missing",
        5 => "Access denied",
        _ => "Can't read",
    }
}

/// `true` when `bytes` contains a PDF name token `/Encrypt` (not
/// `/Encryption` or a longer name).
pub fn contains_pdf_encrypt(bytes: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"/Encrypt";
    let mut i = 0;
    while i + NEEDLE.len() <= bytes.len() {
        if &bytes[i..i + NEEDLE.len()] == NEEDLE {
            let after = i + NEEDLE.len();
            if after == bytes.len() || is_pdf_delimiter(bytes[after]) {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn is_pdf_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

/// Read at most [`PDF_ENCRYPT_SCAN_WINDOW`] from each end. A short file is
/// read once. Used only after a PDF load has already failed.
pub fn read_encrypt_probe(path: &Path) -> std::io::Result<bool> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let window = PDF_ENCRYPT_SCAN_WINDOW;
    if len <= window.saturating_mul(2) {
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        return Ok(contains_pdf_encrypt(&buf));
    }
    let mut head = vec![0_u8; window as usize];
    let n = read_at(&mut file, &mut head)?;
    head.truncate(n);
    let tail_len = window.min(len);
    file.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0_u8; tail_len as usize];
    let n = read_at(&mut file, &mut tail)?;
    tail.truncate(n);
    Ok(contains_pdf_encrypt(&head) || contains_pdf_encrypt(&tail))
}

fn read_at(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(filled)
}

/// `1 page · PDF` or `12 pages · PDF`. Zero pages omits the count.
pub fn format_pdf_pages(pages: u32) -> String {
    match pages {
        0 => "PDF".to_string(),
        1 => "1 page · PDF".to_string(),
        n => format!("{} pages · PDF", format_grouped(u64::from(n))),
    }
}

/// Header details once the page is up: `12 KB · 612 × 792 · 12 pages · PDF`.
///
/// Dimensions are the page size in DIPs (pixels at 96 DPI), which is what
/// actual size (`1`) is based on. The stored bitmap may be smaller.
pub fn format_pdf_header(total_bytes: u64, width: u32, height: u32, pages: u32) -> String {
    format!(
        "{} · {} × {} · {}",
        format_compact_size(total_bytes),
        format_grouped(u64::from(width)),
        format_grouped(u64::from(height)),
        format_pdf_pages(pages)
    )
}

/// Page size in DIPs, and the bitmap size to render.
///
/// `source_*` is the page in DIPs, rounded. The image view turns that into
/// actual size with `source * dpi / 96`. `render_*` fits the DPI-scaled page
/// into `target` without scaling a smaller page up, then clamps to the pixel
/// cap. Zero, negative, and non-finite page sizes are [`PdfError::InvalidSize`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfPagePx {
    pub source_width: u32,
    pub source_height: u32,
    pub render_width: u32,
    pub render_height: u32,
}

pub fn pdf_page_pixels(
    page_w_dip: f32,
    page_h_dip: f32,
    target: PdfTarget,
    limits: &PdfLimits,
) -> Result<PdfPagePx, PdfError> {
    if !page_w_dip.is_finite() || !page_h_dip.is_finite() || page_w_dip <= 0.0 || page_h_dip <= 0.0
    {
        return Err(PdfError::InvalidSize);
    }
    let source_width = dip_px(page_w_dip)?;
    let source_height = dip_px(page_h_dip)?;
    let dpi = f64::from(effective_dpi(target.dpi));
    let nat_w = (f64::from(page_w_dip) * dpi / 96.0).round();
    let nat_h = (f64::from(page_h_dip) * dpi / 96.0).round();
    if !nat_w.is_finite() || !nat_h.is_finite() || nat_w < 1.0 || nat_h < 1.0 {
        return Err(PdfError::InvalidSize);
    }
    let nat_w = clamp_u32(nat_w);
    let nat_h = clamp_u32(nat_h);
    if nat_w == 0 || nat_h == 0 {
        return Err(PdfError::InvalidSize);
    }
    let max_side = limits.max_dimension.max(1);
    let box_w = target.width.max(1).min(max_side);
    let box_h = target.height.max(1).min(max_side);
    let (fit_w, fit_h) = fit_size(nat_w, nat_h, box_w, box_h);
    if fit_w == 0 || fit_h == 0 {
        return Err(PdfError::InvalidSize);
    }
    let (render_width, render_height) = clamp_render(fit_w, fit_h, max_side, limits.max_pixels)?;
    Ok(PdfPagePx {
        source_width,
        source_height,
        render_width,
        render_height,
    })
}

fn dip_px(dip: f32) -> Result<u32, PdfError> {
    let rounded = f64::from(dip).round();
    if !rounded.is_finite() || rounded < 1.0 {
        return Err(PdfError::InvalidSize);
    }
    let px = clamp_u32(rounded);
    if px == 0 {
        Err(PdfError::InvalidSize)
    } else {
        Ok(px)
    }
}

fn clamp_u32(value: f64) -> u32 {
    if !value.is_finite() || value <= 0.0 {
        return 0;
    }
    if value >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        value as u32
    }
}

/// Scale `width` × `height` down so each side is at most `max_side` and the
/// product is at most `max_pixels`. Never scales up.
fn clamp_render(
    width: u32,
    height: u32,
    max_side: u32,
    max_pixels: u64,
) -> Result<(u32, u32), PdfError> {
    if width == 0 || height == 0 || max_side == 0 || max_pixels == 0 {
        return Err(PdfError::InvalidSize);
    }
    let mut scale = 1.0_f64;
    let w = f64::from(width);
    let h = f64::from(height);
    let max_side = f64::from(max_side);
    if w > max_side {
        scale = scale.min(max_side / w);
    }
    if h > max_side {
        scale = scale.min(max_side / h);
    }
    let pixels = w * h;
    if pixels * scale * scale > max_pixels as f64 {
        scale = scale.min(((max_pixels as f64) / pixels).sqrt());
    }
    if !scale.is_finite() || scale <= 0.0 {
        return Err(PdfError::InvalidSize);
    }
    let rw = (w * scale).floor().max(1.0);
    let rh = (h * scale).floor().max(1.0);
    let rw = clamp_u32(rw).max(1);
    let rh = clamp_u32(rh).max(1);
    let product = u64::from(rw).saturating_mul(u64::from(rh));
    if product > max_pixels || rw > max_side as u32 || rh > max_side as u32 {
        // Floor can leave one side a pixel over the cap. Step down once.
        let again = scale * 0.99;
        let rw = clamp_u32((w * again).floor().max(1.0)).clamp(1, max_side as u32);
        let rh = clamp_u32((h * again).floor().max(1.0)).clamp(1, max_side as u32);
        if u64::from(rw).saturating_mul(u64::from(rh)) > max_pixels {
            return Err(PdfError::TooManyPixels);
        }
        return Ok((rw, rh));
    }
    Ok((rw, rh))
}

/// How decoded BGRA samples store alpha.
///
/// [`PdfSampleAlpha::Ignored`] matches `BitmapAlphaMode::Ignore`: the decoder
/// keeps the color and writes 0 into the alpha channel. Compositing those
/// samples on white turns the page white, so they are copied instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfSampleAlpha {
    /// Color is stored independently of alpha.
    Straight,
    /// Color is already multiplied by alpha.
    Premultiplied,
    /// Alpha was discarded. The color channels are the opaque pixel.
    Ignored,
}

/// Map a WinRT `BitmapAlphaMode` value (`Premultiplied` = 0, `Straight` = 1,
/// `Ignore` = 2). Any other value is treated as ignored so a zero alpha
/// channel cannot wipe the color.
pub fn pdf_sample_alpha(bitmap_alpha_mode: i32) -> PdfSampleAlpha {
    match bitmap_alpha_mode {
        0 => PdfSampleAlpha::Premultiplied,
        1 => PdfSampleAlpha::Straight,
        _ => PdfSampleAlpha::Ignored,
    }
}

/// Reject a decoded bitmap before its pixels are allocated.
///
/// A zero side is [`PdfError::InvalidSize`]. A side above
/// `limits.max_dimension`, or a product above the smaller of
/// `limits.max_pixels` and [`PDF_MAX_PIXELS`], is [`PdfError::TooManyPixels`].
/// A size equal to the cap is allowed. The render request is already clamped;
/// this catches a decoder that ignored that size.
pub fn gate_pdf_bitmap(width: u32, height: u32, limits: &PdfLimits) -> Result<(), PdfError> {
    if width == 0 || height == 0 {
        return Err(PdfError::InvalidSize);
    }
    let max_side = limits.max_dimension.max(1);
    if width > max_side || height > max_side {
        return Err(PdfError::TooManyPixels);
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    let cap = limits.max_pixels.min(PDF_MAX_PIXELS);
    if pixels > cap {
        return Err(PdfError::TooManyPixels);
    }
    Ok(())
}

/// Copy BGRA rows into a tightly packed opaque buffer.
///
/// Straight and premultiplied samples are composited on white. Ignored
/// samples copy RGB and store alpha 255, because `BitmapAlphaMode::Ignore`
/// writes 0 into the alpha channel while leaving the color intact.
///
/// `start` is the first byte of row 0. `stride` may be negative (bottom-up).
/// Out-of-range rows are [`PdfError::Damaged`]. The product is capped at
/// [`PDF_MAX_PIXELS`].
pub fn pack_bgra_rows(
    src: &[u8],
    start: i32,
    width: u32,
    height: u32,
    stride: i32,
    alpha: PdfSampleAlpha,
) -> Result<Vec<u8>, PdfError> {
    if width == 0 || height == 0 || stride == 0 {
        return Err(PdfError::InvalidSize);
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > PDF_MAX_PIXELS {
        return Err(PdfError::TooManyPixels);
    }
    let row_bytes = u64::from(width).saturating_mul(4);
    let mut out = Vec::new();
    let reserve = usize::try_from(pixels.saturating_mul(4)).map_err(|_| PdfError::TooManyPixels)?;
    out.try_reserve(reserve)
        .map_err(|_| PdfError::TooManyPixels)?;
    let start = i64::from(start);
    let stride = i64::from(stride);
    let row_bytes_i = i64::try_from(row_bytes).map_err(|_| PdfError::TooManyPixels)?;
    for y in 0..i64::from(height) {
        let row = start + y * stride;
        if row < 0 {
            return Err(PdfError::Damaged);
        }
        let row = usize::try_from(row).map_err(|_| PdfError::Damaged)?;
        let end = row
            .checked_add(usize::try_from(row_bytes_i).map_err(|_| PdfError::Damaged)?)
            .ok_or(PdfError::Damaged)?;
        if end > src.len() {
            return Err(PdfError::Damaged);
        }
        for px in src[row..end].chunks_exact(4) {
            let (b, g, r) = match alpha {
                PdfSampleAlpha::Straight => (
                    over_white(px[0], px[3]),
                    over_white(px[1], px[3]),
                    over_white(px[2], px[3]),
                ),
                PdfSampleAlpha::Premultiplied => (
                    over_white_premultiplied(px[0], px[3]),
                    over_white_premultiplied(px[1], px[3]),
                    over_white_premultiplied(px[2], px[3]),
                ),
                PdfSampleAlpha::Ignored => (px[0], px[1], px[2]),
            };
            out.push(b);
            out.push(g);
            out.push(r);
            out.push(255);
        }
    }
    Ok(out)
}

fn over_white(channel: u8, alpha: u8) -> u8 {
    let channel = u32::from(channel);
    let alpha = u32::from(alpha);
    let inv = 255 - alpha;
    let mixed = (channel * alpha + 255 * inv + 127) / 255;
    u8::try_from(mixed).unwrap_or(255)
}

/// `stored + 255 * (1 - alpha/255)`, which is `stored + (255 - alpha)`.
fn over_white_premultiplied(channel: u8, alpha: u8) -> u8 {
    channel.saturating_add(255 - alpha)
}

/// Preview value for a rendered page. `source_*` on `image` is the DIP size.
pub fn preview_from_page(total_bytes: u64, image: DecodedImage, pages: u32) -> Preview {
    debug_assert_eq!(image.format, ImageFormat::Pdf);
    Preview {
        content: String::new(),
        truncated: false,
        line_count: 0,
        truncation: None,
        total_bytes,
        image: Some(std::sync::Arc::new(image)),
        markdown_rtf: None,
        pdf_pages: pages,
    }
}

/// Which wait the worker is in. Open and render share one budget and one
/// cancel rule; the phase is named so a test can say which wait was cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfJobPhase {
    Opening,
    Rendering,
}

/// What the worker should do with the in-flight `IAsyncOperation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfJobDecision {
    /// Keep waiting. The budget has not been spent and the job is current.
    Wait,
    /// `IAsyncInfo.Cancel`, then drop whatever comes back. No page, no
    /// timeout message.
    CancelAndDiscard,
    /// `IAsyncInfo.Cancel`, then the icon stays with a timeout reason.
    CancelAndTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PdfJobSnap {
    pub phase: PdfJobPhase,
    pub job_gen: Generation,
    pub current_gen: Generation,
    pub cancel_requested: bool,
    pub elapsed: Duration,
    pub budget: Duration,
}

pub fn pdf_job_decision(job: PdfJobSnap) -> PdfJobDecision {
    // Both waits are the same: a newer file or Esc discards, and the clock
    // covers open plus render together.
    match job.phase {
        PdfJobPhase::Opening | PdfJobPhase::Rendering => {}
    }
    if job.cancel_requested || job.job_gen != job.current_gen {
        PdfJobDecision::CancelAndDiscard
    } else if job.elapsed >= job.budget {
        PdfJobDecision::CancelAndTimeout
    } else {
        PdfJobDecision::Wait
    }
}

/// Result of applying [`pdf_job_decision`] to a finished or still-running op.
/// A bitmap that arrives after the budget, or after cancel, is not shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfSettle {
    KeepWaiting,
    ShowPage,
    Discard,
    Fallback(PdfError),
}

pub fn settle_pdf_job(job: PdfJobSnap, completed: bool) -> PdfSettle {
    match pdf_job_decision(job) {
        PdfJobDecision::CancelAndDiscard => PdfSettle::Discard,
        PdfJobDecision::CancelAndTimeout => PdfSettle::Fallback(PdfError::TimedOut),
        PdfJobDecision::Wait if completed => PdfSettle::ShowPage,
        PdfJobDecision::Wait => PdfSettle::KeepWaiting,
    }
}

/// What the preview is holding for the current PDF, if any.
///
/// [`PdfView::Icon`] is the immediate outcome (shell icon and quick info).
/// A later [`PdfViewEvent::PageReady`] with the same generation replaces it.
/// Esc, a new file, or leaving the PDF drops a late bitmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfView {
    Idle,
    Icon { gen: Generation },
    Page { gen: Generation },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfViewEvent {
    /// Space or a selection landed on a PDF. Show the icon now.
    Begin(Generation),
    /// The worker produced a page for `gen`.
    PageReady(Generation),
    /// Esc, or the window closed, while this PDF was current.
    Cancel,
    /// The session left the PDF (another format, or the preview closed)
    /// and any bitmap must be dropped.
    Release,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfViewEffect {
    ShowIcon,
    ShowPage,
    DropBitmap,
    CancelInFlight,
}

pub fn step_pdf_view(view: PdfView, event: PdfViewEvent) -> (PdfView, Vec<PdfViewEffect>) {
    match event {
        PdfViewEvent::Begin(gen) => {
            let mut effects = Vec::new();
            if !matches!(view, PdfView::Idle) {
                effects.push(PdfViewEffect::CancelInFlight);
                effects.push(PdfViewEffect::DropBitmap);
            }
            effects.push(PdfViewEffect::ShowIcon);
            (PdfView::Icon { gen }, effects)
        }
        PdfViewEvent::PageReady(gen) => match view {
            PdfView::Icon { gen: current } if current == gen => {
                (PdfView::Page { gen }, vec![PdfViewEffect::ShowPage])
            }
            other => (other, vec![]),
        },
        PdfViewEvent::Cancel => match view {
            PdfView::Idle => (PdfView::Idle, vec![]),
            _ => (
                PdfView::Idle,
                vec![PdfViewEffect::CancelInFlight, PdfViewEffect::DropBitmap],
            ),
        },
        PdfViewEvent::Release => match view {
            PdfView::Idle => (PdfView::Idle, vec![]),
            PdfView::Icon { .. } => (
                PdfView::Idle,
                vec![PdfViewEffect::CancelInFlight, PdfViewEffect::DropBitmap],
            ),
            PdfView::Page { .. } => (PdfView::Idle, vec![PdfViewEffect::DropBitmap]),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn limits() -> PdfLimits {
        PdfLimits::default()
    }

    fn target(width: u32, height: u32, dpi: u32) -> PdfTarget {
        PdfTarget { width, height, dpi }
    }

    fn snap(
        phase: PdfJobPhase,
        elapsed_ms: u64,
        cancel: bool,
        job: u64,
        current: u64,
    ) -> PdfJobSnap {
        PdfJobSnap {
            phase,
            job_gen: Generation::new(job),
            current_gen: Generation::new(current),
            cancel_requested: cancel,
            elapsed: Duration::from_millis(elapsed_ms),
            budget: PDF_TIMEOUT,
        }
    }

    #[test]
    fn pdf_extension_is_case_insensitive_and_not_pdfx() {
        assert!(is_pdf_path(Path::new("notes.pdf")));
        assert!(is_pdf_path(Path::new("notes.PDF")));
        assert!(is_pdf_path(Path::new(r"C:\docs\Notes.Pdf")));
        assert!(is_pdf_path(Path::new(".pdf")));
        assert!(is_pdf_path(Path::new(".PDF")));
        assert!(!is_pdf_path(Path::new("notes.pdfx")));
        assert!(!is_pdf_path(Path::new("notes.pdf.txt")));
        assert!(!is_pdf_path(Path::new("notes.txt")));
        assert!(!is_pdf_path(Path::new("Makefile")));
        assert!(!is_pdf_path(Path::new("icon.svg")));
        assert!(!is_pdf_path(Path::new("photo.png")));
        assert!(!is_pdf_path(Path::new("readme.md")));
    }

    #[test]
    fn sniff_matches_prefix_only_and_only_for_extensionless_files() {
        assert!(sniff_pdf(b"%PDF-1.7"));
        assert!(sniff_pdf(b"%PDF"));
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(b"%PDF-1.4");
        assert!(sniff_pdf(&bom));
        assert!(!sniff_pdf(b" %PDF"));
        assert!(!sniff_pdf(b"hello %PDF"));
        assert!(!sniff_pdf(b"%PD"));
        assert!(!sniff_pdf(b""));
        assert!(!sniff_pdf(b"<svg"));

        assert!(extensionless_pdf(Path::new("scan"), b"%PDF-1.4"));
        assert!(extensionless_pdf(Path::new("Makefile"), b"%PDF-1.4"));
        assert!(!extensionless_pdf(Path::new("notes.txt"), b"%PDF-1.4"));
        assert!(!extensionless_pdf(Path::new("photo.png"), b"%PDF-1.4"));
        assert!(!extensionless_pdf(Path::new("icon.svg"), b"%PDF-1.4"));
        assert!(!extensionless_pdf(Path::new("notes.pdf"), b"%PDF-1.4"));
        assert!(!extensionless_pdf(Path::new("notes.pdfx"), b"%PDF-1.4"));
        assert!(!extensionless_pdf(Path::new("scan"), b"hello"));
    }

    #[test]
    fn file_gate_is_empty_too_large_or_render() {
        assert_eq!(gate_pdf_file(0, &limits()), PdfGate::Empty);
        assert_eq!(gate_pdf_file(1, &limits()), PdfGate::Render);
        assert_eq!(
            gate_pdf_file(PDF_MAX_FILE_BYTES, &limits()),
            PdfGate::Render
        );
        assert_eq!(
            gate_pdf_file(PDF_MAX_FILE_BYTES + 1, &limits()),
            PdfGate::TooLarge {
                bytes: PDF_MAX_FILE_BYTES + 1
            }
        );
        assert!(limits().max_pixels <= 32_000_000);
        assert!(limits().max_pixels <= crate::image_preview::ImageLimits::default().max_pixels);
        assert_eq!(limits().timeout, Duration::from_millis(1_500));
        assert_eq!(limits().max_dimension, 4096);
    }

    #[test]
    fn header_uses_page_singular_and_plural() {
        assert_eq!(format_pdf_pages(0), "PDF");
        assert_eq!(format_pdf_pages(1), "1 page · PDF");
        assert_eq!(format_pdf_pages(12), "12 pages · PDF");
        assert_eq!(format_pdf_pages(1_000), "1,000 pages · PDF");
        assert_eq!(
            format_pdf_header(12 * 1024, 612, 792, 12),
            "12 KB · 612 × 792 · 12 pages · PDF"
        );
        assert_eq!(
            format_pdf_header(2048, 612, 792, 1),
            "2 KB · 612 × 792 · 1 page · PDF"
        );
    }

    #[test]
    fn messages_and_header_status_for_each_fallback() {
        let name = "notes.pdf";
        assert_eq!(PdfError::Rendering.user_message(name), "");
        assert_eq!(PdfError::Rendering.header_status(), "PDF");
        assert!(PdfError::Rendering.is_placeholder());

        let password = PdfError::PasswordProtected;
        assert_eq!(
            password.user_message(name),
            "notes.pdf can't be previewed — this PDF is password protected"
        );
        assert_eq!(password.header_status(), "Password protected");

        let damaged = PdfError::Damaged;
        assert_eq!(
            damaged.user_message(name),
            "notes.pdf can't be previewed — this PDF looks damaged or isn't a valid PDF"
        );
        assert_eq!(damaged.header_status(), "Damaged");

        let large = PdfError::FileTooLarge {
            bytes: 128 * 1024 * 1024,
        };
        assert_eq!(
            large.user_message(name),
            "notes.pdf can't be previewed — this PDF is too large (128 MB)"
        );
        assert_eq!(large.header_status(), "Too large");

        assert_eq!(
            PdfError::TimedOut.user_message(name),
            "notes.pdf can't be previewed — this PDF took too long to open"
        );
        assert_eq!(PdfError::TimedOut.header_status(), "Timed out");
        assert_eq!(PdfError::TooManyPixels.header_status(), "Too large");
        assert_eq!(
            PdfError::TooManyPixels.user_message(name),
            "notes.pdf can't be previewed — this PDF page is too large to preview"
        );
        assert_eq!(PdfError::InvalidSize.header_status(), "Invalid size");
        assert_eq!(
            PdfError::InvalidSize.user_message(""),
            "This file can't be previewed — this PDF page has an invalid size"
        );
        assert_eq!(PdfError::Unavailable.header_status(), "Unavailable");
        assert!(!password.is_placeholder());
    }

    #[test]
    fn encrypt_token_is_a_name_not_a_prefix() {
        assert!(contains_pdf_encrypt(
            b"%PDF-1.6\ntrailer <</Encrypt 2 0 R>>"
        ));
        assert!(contains_pdf_encrypt(b"/Encrypt"));
        assert!(contains_pdf_encrypt(b"/Encrypt/Filter"));
        assert!(!contains_pdf_encrypt(b"%PDF-1.6\ntrailer <</Size 3>>"));
        assert!(!contains_pdf_encrypt(b"/Encryption"));
        assert!(!contains_pdf_encrypt(b"/Encryptor 1 0 R"));
        assert!(!contains_pdf_encrypt(b"Encrypt"));
        assert!(!contains_pdf_encrypt(b""));
    }

    #[test]
    fn encrypt_probe_reads_a_synthetic_file_and_a_tail() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain.pdf");
        std::fs::write(&plain, b"%PDF-1.4\n1 0 obj << /Size 2 >> endobj\n%%EOF").unwrap();
        assert!(!read_encrypt_probe(&plain).unwrap());

        let locked = dir.path().join("locked.pdf");
        std::fs::write(
            &locked,
            b"%PDF-1.4\n1 0 obj << /Encrypt 2 0 R >> endobj\n%%EOF",
        )
        .unwrap();
        assert!(read_encrypt_probe(&locked).unwrap());

        let huge = dir.path().join("tail.pdf");
        let mut file = File::create(&huge).unwrap();
        file.write_all(b"%PDF-1.7\n").unwrap();
        let pad = vec![b'x'; (PDF_ENCRYPT_SCAN_WINDOW as usize) * 2 + 64];
        file.write_all(&pad).unwrap();
        file.write_all(b"\ntrailer << /Encrypt 5 0 R >>\n%%EOF")
            .unwrap();
        drop(file);
        assert!(read_encrypt_probe(&huge).unwrap());

        assert_eq!(
            classify_pdf_open_failure(None, true),
            PdfError::PasswordProtected
        );
        assert_eq!(
            classify_pdf_open_failure(Some(true), false),
            PdfError::PasswordProtected
        );
        assert_eq!(
            classify_pdf_open_failure(Some(false), false),
            PdfError::Damaged
        );
        assert_eq!(classify_pdf_open_failure(None, false), PdfError::Damaged);
    }

    #[test]
    fn activation_hresults_are_unavailable_and_other_codes_are_damaged() {
        assert!(is_pdf_engine_unavailable(HRESULT_CLASSNOTREG));
        assert!(is_pdf_engine_unavailable(HRESULT_CLASSNOTAVAILABLE));
        assert!(is_pdf_engine_unavailable(HRESULT_NOT_INITIALIZED));
        assert!(!is_pdf_engine_unavailable(0));
        // E_FAIL. A failed load, not a missing PDF engine.
        assert!(!is_pdf_engine_unavailable(0x8000_4005_u32 as i32));
        assert_eq!(
            pdf_error_from_hresult(HRESULT_NOT_INITIALIZED),
            PdfError::Unavailable
        );
        assert_eq!(
            pdf_error_from_hresult(0x8000_4005_u32 as i32),
            PdfError::Damaged
        );
    }

    #[test]
    fn sharing_missing_and_access_denied_are_not_a_damaged_pdf() {
        let hr = |code: u32| (0x8007_0000 | code) as i32;
        assert_eq!(pdf_error_from_hresult(hr(32)), PdfError::Io(32));
        assert_eq!(pdf_error_from_hresult(hr(33)), PdfError::Io(33));
        assert_eq!(pdf_error_from_hresult(hr(5)), PdfError::Io(5));
        assert_eq!(pdf_error_from_hresult(hr(2)), PdfError::Io(2));
        assert_eq!(pdf_error_from_hresult(hr(3)), PdfError::Io(3));
        // Some other Win32 code is still a bad file, and so is facility 7
        // with a zero code.
        assert_eq!(pdf_error_from_hresult(hr(87)), PdfError::Damaged);
        assert_eq!(
            pdf_error_from_hresult(0x8007_0000_u32 as i32),
            PdfError::Damaged
        );

        assert_eq!(
            pdf_error_from_io(&std::io::Error::from_raw_os_error(32)),
            PdfError::Io(32)
        );
        assert_eq!(
            pdf_error_from_io(&std::io::Error::from_raw_os_error(33)),
            PdfError::Io(33)
        );
        assert_eq!(
            pdf_error_from_io(&std::io::Error::new(std::io::ErrorKind::NotFound, "gone")),
            PdfError::Io(2)
        );
        assert_eq!(
            pdf_error_from_io(&std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "no"
            )),
            PdfError::Io(5)
        );
        assert_eq!(
            pdf_error_from_io(&std::io::Error::other("disk")),
            PdfError::Damaged
        );

        assert_eq!(
            PdfError::Io(32).user_message("notes.pdf"),
            "notes.pdf is open in another program and can't be read"
        );
        assert_eq!(PdfError::Io(32).header_status(), "In use");
        assert_eq!(PdfError::Io(2).header_status(), "Missing");
        assert_eq!(PdfError::Io(5).header_status(), "Access denied");
        assert_eq!(
            PdfError::Io(5).user_message(""),
            "Access to This file was denied"
        );
        assert!(!PdfError::Io(32).is_placeholder());
    }

    #[test]
    fn small_page_is_not_upscaled_and_dip_is_the_source_size() {
        let page = pdf_page_pixels(612.0, 792.0, target(2_000, 2_000, 96), &limits()).unwrap();
        assert_eq!(page.source_width, 612);
        assert_eq!(page.source_height, 792);
        assert_eq!(page.render_width, 612);
        assert_eq!(page.render_height, 792);

        let small = pdf_page_pixels(50.0, 50.0, target(800, 800, 96), &limits()).unwrap();
        assert_eq!((small.render_width, small.render_height), (50, 50));
        assert_eq!((small.source_width, small.source_height), (50, 50));
    }

    #[test]
    fn large_page_fits_the_body_without_changing_aspect() {
        let page = pdf_page_pixels(612.0, 792.0, target(400, 400, 96), &limits()).unwrap();
        assert!(page.render_width <= 400);
        assert!(page.render_height <= 400);
        assert_eq!(page.render_height, 400);
        assert!(page.render_width < 400);
        assert_eq!((page.source_width, page.source_height), (612, 792));
    }

    #[test]
    fn dpi_scales_the_bitmap_and_keeps_dips_as_the_source() {
        let page = pdf_page_pixels(100.0, 50.0, target(1_000, 1_000, 192), &limits()).unwrap();
        assert_eq!((page.source_width, page.source_height), (100, 50));
        assert_eq!((page.render_width, page.render_height), (200, 100));

        let fitted = pdf_page_pixels(1_000.0, 1_000.0, target(100, 100, 192), &limits()).unwrap();
        assert_eq!((fitted.source_width, fitted.source_height), (1000, 1000));
        assert_eq!((fitted.render_width, fitted.render_height), (100, 100));
    }

    #[test]
    fn invalid_page_sizes_are_errors() {
        let body = target(800, 600, 96);
        for (w, h) in [
            (0.0, 100.0),
            (100.0, 0.0),
            (-4.0, 100.0),
            (100.0, -1.0),
            (f32::NAN, 100.0),
            (100.0, f32::INFINITY),
            (f32::NEG_INFINITY, 10.0),
        ] {
            assert_eq!(
                pdf_page_pixels(w, h, body, &limits()),
                Err(PdfError::InvalidSize),
                "{w} x {h}"
            );
        }
    }

    #[test]
    fn huge_page_is_clamped_to_the_pixel_cap() {
        let page = pdf_page_pixels(1.0e7, 1.0e7, target(8_000, 8_000, 96), &limits()).unwrap();
        assert!(page.render_width <= 4096);
        assert!(page.render_height <= 4096);
        let pixels = u64::from(page.render_width) * u64::from(page.render_height);
        assert!(pixels <= PDF_MAX_PIXELS);
        assert!(pixels >= 1);
        assert!(page.source_width >= 1);
        assert!(page.source_height >= 1);

        let wide = pdf_page_pixels(1.0e6, 10.0, target(8_000, 8_000, 96), &limits()).unwrap();
        assert!(wide.render_width <= 4096);
        assert!(u64::from(wide.render_width) * u64::from(wide.render_height) <= PDF_MAX_PIXELS);
    }

    #[test]
    fn packed_rows_composite_on_white_and_honor_stride() {
        // Two pixels, opaque red then half-transparent black, stride padded.
        let src = [
            0, 0, 255, 255, // BGRA red
            0, 0, 0, 128, // BGRA black, half alpha
            1, 2, 3, 4, // padding
        ];
        let packed = pack_bgra_rows(&src, 0, 2, 1, 12, PdfSampleAlpha::Straight).unwrap();
        assert_eq!(packed.len(), 8);
        assert_eq!(&packed[0..4], &[0, 0, 255, 255]);
        // Half-transparent black on white is gray, not black and not white.
        assert_eq!(&packed[4..8], &[127, 127, 127, 255]);

        let bottom_up = [
            0, 255, 0, 255, // row 1 green
            255, 0, 0, 255, // row 0 blue, stored second
        ];
        let flipped = pack_bgra_rows(&bottom_up, 4, 1, 2, -4, PdfSampleAlpha::Straight).unwrap();
        assert_eq!(&flipped[0..4], &[255, 0, 0, 255]);
        assert_eq!(&flipped[4..8], &[0, 255, 0, 255]);

        assert_eq!(
            pack_bgra_rows(&src, 0, 0, 1, 4, PdfSampleAlpha::Straight),
            Err(PdfError::InvalidSize)
        );
        assert_eq!(
            pack_bgra_rows(&src, 0, 4, 1, 4, PdfSampleAlpha::Straight),
            Err(PdfError::Damaged)
        );
        assert_eq!(
            pack_bgra_rows(&src, -4, 1, 1, 4, PdfSampleAlpha::Straight),
            Err(PdfError::Damaged)
        );
    }

    #[test]
    fn ignored_alpha_keeps_the_color_instead_of_painting_white() {
        // BitmapAlphaMode::Ignore stores the color and sets alpha to 0.
        let src = [0, 0, 255, 0];
        let packed = pack_bgra_rows(&src, 0, 1, 1, 4, PdfSampleAlpha::Ignored).unwrap();
        assert_eq!(packed, vec![0, 0, 255, 255]);
        // A non-zero leftover alpha is still not a coverage value.
        let leftover = [0, 0, 255, 128];
        let packed = pack_bgra_rows(&leftover, 0, 1, 1, 4, PdfSampleAlpha::Ignored).unwrap();
        assert_eq!(packed, vec![0, 0, 255, 255]);
    }

    #[test]
    fn premultiplied_samples_composite_on_white() {
        let half_black = [0, 0, 0, 128];
        let packed =
            pack_bgra_rows(&half_black, 0, 1, 1, 4, PdfSampleAlpha::Premultiplied).unwrap();
        assert_eq!(packed, vec![127, 127, 127, 255]);

        // 50% red, already multiplied: stored red 128, alpha 128, on white.
        let half_red = [0, 0, 128, 128];
        let packed = pack_bgra_rows(&half_red, 0, 1, 1, 4, PdfSampleAlpha::Premultiplied).unwrap();
        assert_eq!(packed, vec![127, 127, 255, 255]);

        let clear = [0, 0, 0, 0];
        let packed = pack_bgra_rows(&clear, 0, 1, 1, 4, PdfSampleAlpha::Premultiplied).unwrap();
        assert_eq!(packed, vec![255, 255, 255, 255]);
    }

    #[test]
    fn winrt_bitmap_alpha_mode_selects_the_pack() {
        assert_eq!(pdf_sample_alpha(0), PdfSampleAlpha::Premultiplied);
        assert_eq!(pdf_sample_alpha(1), PdfSampleAlpha::Straight);
        assert_eq!(pdf_sample_alpha(2), PdfSampleAlpha::Ignored);
        assert_eq!(pdf_sample_alpha(-1), PdfSampleAlpha::Ignored);
        assert_eq!(pdf_sample_alpha(99), PdfSampleAlpha::Ignored);
    }

    #[test]
    fn decoded_bitmap_is_rejected_before_pixels_are_allocated() {
        let limits = PdfLimits::default();
        assert_eq!(limits.max_dimension, 4096);
        assert_eq!(gate_pdf_bitmap(1, 1, &limits), Ok(()));
        assert_eq!(gate_pdf_bitmap(4096, 4096, &limits), Ok(()));
        assert_eq!(gate_pdf_bitmap(0, 10, &limits), Err(PdfError::InvalidSize));
        assert_eq!(gate_pdf_bitmap(10, 0, &limits), Err(PdfError::InvalidSize));
        assert_eq!(
            gate_pdf_bitmap(4097, 1, &limits),
            Err(PdfError::TooManyPixels)
        );
        assert_eq!(
            gate_pdf_bitmap(1, 4097, &limits),
            Err(PdfError::TooManyPixels)
        );

        let tight = PdfLimits {
            max_pixels: 100,
            ..PdfLimits::default()
        };
        assert_eq!(gate_pdf_bitmap(10, 10, &tight), Ok(()));
        assert_eq!(
            gate_pdf_bitmap(11, 10, &tight),
            Err(PdfError::TooManyPixels)
        );

        // A raised caller cap cannot exceed the absolute pixel ceiling.
        let raised = PdfLimits {
            max_pixels: PDF_MAX_PIXELS + 1,
            max_dimension: 20_000,
            ..PdfLimits::default()
        };
        assert_eq!(gate_pdf_bitmap(5_000, 5_000, &raised), Ok(()));
        // 7000 * 5000 = 35_000_000, both sides under 20_000, over 32_000_000.
        assert_eq!(
            gate_pdf_bitmap(7_000, 5_000, &raised),
            Err(PdfError::TooManyPixels)
        );
        assert_eq!(
            gate_pdf_bitmap(6_000, 6_000, &raised),
            Err(PdfError::TooManyPixels)
        );
    }

    #[test]
    fn cancel_during_open_and_render_discards_even_if_a_bitmap_exists() {
        for phase in [PdfJobPhase::Opening, PdfJobPhase::Rendering] {
            let job = snap(phase, 10, true, 1, 1);
            assert_eq!(pdf_job_decision(job), PdfJobDecision::CancelAndDiscard);
            assert_eq!(settle_pdf_job(job, true), PdfSettle::Discard);
            assert_eq!(settle_pdf_job(job, false), PdfSettle::Discard);
        }
    }

    #[test]
    fn a_newer_generation_discards_the_in_flight_job() {
        let job = snap(PdfJobPhase::Rendering, 10, false, 1, 2);
        assert_eq!(pdf_job_decision(job), PdfJobDecision::CancelAndDiscard);
        assert_eq!(settle_pdf_job(job, true), PdfSettle::Discard);
    }

    #[test]
    fn timeout_is_a_fallback_and_does_not_show_a_late_page() {
        for phase in [PdfJobPhase::Opening, PdfJobPhase::Rendering] {
            let at = snap(phase, 1_500, false, 3, 3);
            assert_eq!(pdf_job_decision(at), PdfJobDecision::CancelAndTimeout);
            assert_eq!(
                settle_pdf_job(at, true),
                PdfSettle::Fallback(PdfError::TimedOut)
            );
            let over = snap(phase, 1_501, false, 3, 3);
            assert_eq!(
                settle_pdf_job(over, true),
                PdfSettle::Fallback(PdfError::TimedOut)
            );
        }
        let early = snap(PdfJobPhase::Opening, 1_499, false, 3, 3);
        assert_eq!(pdf_job_decision(early), PdfJobDecision::Wait);
        assert_eq!(settle_pdf_job(early, false), PdfSettle::KeepWaiting);
        assert_eq!(settle_pdf_job(early, true), PdfSettle::ShowPage);
    }

    #[test]
    fn cancel_wins_over_a_timeout() {
        let job = snap(PdfJobPhase::Rendering, 5_000, true, 1, 1);
        assert_eq!(pdf_job_decision(job), PdfJobDecision::CancelAndDiscard);
        assert_eq!(settle_pdf_job(job, true), PdfSettle::Discard);
    }

    #[test]
    fn icon_is_immediate_and_a_matching_page_replaces_it() {
        let (view, effects) = step_pdf_view(PdfView::Idle, PdfViewEvent::Begin(Generation::new(1)));
        assert_eq!(
            view,
            PdfView::Icon {
                gen: Generation::new(1)
            }
        );
        assert_eq!(effects, vec![PdfViewEffect::ShowIcon]);

        let (view, effects) = step_pdf_view(view, PdfViewEvent::PageReady(Generation::new(1)));
        assert_eq!(
            view,
            PdfView::Page {
                gen: Generation::new(1)
            }
        );
        assert_eq!(effects, vec![PdfViewEffect::ShowPage]);
    }

    #[test]
    fn a_stale_page_does_not_replace_the_icon_or_a_newer_file() {
        let icon = PdfView::Icon {
            gen: Generation::new(2),
        };
        let (view, effects) = step_pdf_view(icon, PdfViewEvent::PageReady(Generation::new(1)));
        assert_eq!(view, icon);
        assert!(effects.is_empty());

        let (view, effects) =
            step_pdf_view(PdfView::Idle, PdfViewEvent::PageReady(Generation::new(1)));
        assert_eq!(view, PdfView::Idle);
        assert!(effects.is_empty());

        let page = PdfView::Page {
            gen: Generation::new(2),
        };
        let (view, effects) = step_pdf_view(page, PdfViewEvent::PageReady(Generation::new(2)));
        assert_eq!(view, page);
        assert!(effects.is_empty());
    }

    #[test]
    fn esc_cancels_and_a_late_bitmap_is_not_shown() {
        let icon = PdfView::Icon {
            gen: Generation::new(4),
        };
        let (view, effects) = step_pdf_view(icon, PdfViewEvent::Cancel);
        assert_eq!(view, PdfView::Idle);
        assert_eq!(
            effects,
            vec![PdfViewEffect::CancelInFlight, PdfViewEffect::DropBitmap]
        );
        let (view, effects) = step_pdf_view(view, PdfViewEvent::PageReady(Generation::new(4)));
        assert_eq!(view, PdfView::Idle);
        assert!(effects.is_empty());
    }

    #[test]
    fn a_new_file_cancels_the_old_render_and_drops_its_bitmap() {
        let page = PdfView::Page {
            gen: Generation::new(1),
        };
        let (view, effects) = step_pdf_view(page, PdfViewEvent::Begin(Generation::new(2)));
        assert_eq!(
            view,
            PdfView::Icon {
                gen: Generation::new(2)
            }
        );
        assert_eq!(
            effects,
            vec![
                PdfViewEffect::CancelInFlight,
                PdfViewEffect::DropBitmap,
                PdfViewEffect::ShowIcon,
            ]
        );
        let (view, effects) = step_pdf_view(view, PdfViewEvent::PageReady(Generation::new(1)));
        assert_eq!(
            view,
            PdfView::Icon {
                gen: Generation::new(2)
            }
        );
        assert!(effects.is_empty());
    }

    #[test]
    fn leaving_a_pdf_drops_the_bitmap_and_ignores_a_late_page() {
        let page = PdfView::Page {
            gen: Generation::new(7),
        };
        let (view, effects) = step_pdf_view(page, PdfViewEvent::Release);
        assert_eq!(view, PdfView::Idle);
        assert_eq!(effects, vec![PdfViewEffect::DropBitmap]);
        let (view, effects) = step_pdf_view(view, PdfViewEvent::PageReady(Generation::new(7)));
        assert_eq!(view, PdfView::Idle);
        assert!(effects.is_empty());

        let icon = PdfView::Icon {
            gen: Generation::new(7),
        };
        let (view, effects) = step_pdf_view(icon, PdfViewEvent::Release);
        assert_eq!(view, PdfView::Idle);
        assert_eq!(
            effects,
            vec![PdfViewEffect::CancelInFlight, PdfViewEffect::DropBitmap]
        );
    }
}
