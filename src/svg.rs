//! SVG preview that does not touch Win32.
//!
//! Detection, caps, parse, and render live here so they run on the loader
//! thread and are tested on Linux. The Windows preview shows the resulting
//! [`DecodedImage`] in the existing image view: fit, zoom, pan, and
//! double-click-to-open are unchanged.
//!
//! Security (enforced in [`svg_options`]):
//! - `resources_dir` is `None`, so relative paths do not resolve against a
//!   directory.
//! - `image_href_resolver.resolve_string` always returns `None`: `file://`,
//!   `http(s)://`, and relative paths are not loaded for `<image>` or
//!   `<feImage>`. `<use href="other.svg#id">` only resolves an id in this
//!   document.
//! - `resolve_data` accepts only `data:` PNG/JPEG/GIF/WebP within
//!   [`SVG_MAX_EMBEDDED_BYTES`]. The header is read without decoding pixels.
//!   An image whose width×height exceeds [`SVG_MAX_EMBEDDED_PIXELS`], or that
//!   would push the document past [`SVG_MAX_EMBEDDED_PIXELS_TOTAL`], is
//!   dropped. A header that cannot be read is dropped. Nested `data:` SVG
//!   is refused.
//! - Gzip is inflated once, up to [`SVGZ_MAX_DECOMPRESSED_BYTES`]. A second
//!   gzip layer is rejected. usvg is given the inflated text via
//!   `Tree::from_str`, so `Tree::from_data`'s own gzip inflate (no size cap)
//!   is never reached.
//! - `@font-face` URLs are not fetched: fonts come only from the process-wide
//!   system [`fontdb`](usvg::fontdb) cache, and only when the SVG contains text.
//!
//! resvg/usvg document that they implement static SVG only: `<script>` is
//! ignored, SMIL animations are ignored, and no events run. roxmltree (used
//! by usvg with `allow_dtd: true`) expands entities with a depth cap of 10
//! and 255 references per reference, so a billion-laughs document fails fast
//! as `EntityReferenceLoop`. External entity resolvers stay unset, so
//! `file://` / `http://` entities are discarded. Deep XML nesting is rejected
//! by a linear pre-scan before roxmltree or usvg run, so a 100k-deep tree
//! cannot overflow the loader stack.

use std::io::{Cursor, Read};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use flate2::read::GzDecoder;
use resvg::tiny_skia::{Color, Pixmap, Transform};
use resvg::usvg::{self, ImageHrefResolver, ImageKind, Options, Tree};

use crate::chrome::{scaled_header_height, LogicalSize};
use crate::geometry::effective_dpi;
use crate::image_preview::{fit_size, DecodedImage, ImageFormat, ImageLimits};
use crate::preview::{format_compact_size, CancelToken, Generation};

/// Refuse a `.svg` (and the compressed `.svgz` file) larger than this.
pub const SVG_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Stop inflating `.svgz` once this many decompressed bytes have been written.
pub const SVGZ_MAX_DECOMPRESSED_BYTES: u64 = 16 * 1024 * 1024;
/// usvg node cap after parse. usvg itself also stops at 1_000_000 XML nodes.
pub const SVG_MAX_NODES: u32 = 200_000;
/// Output pixmap pixel cap. Tighter than the raster image path (100 MP).
pub const SVG_MAX_PIXELS: u64 = 32_000_000;
/// Largest decoded `data:` image payload we will hand to resvg.
pub const SVG_MAX_EMBEDDED_BYTES: u64 = SVG_MAX_FILE_BYTES;
/// One embedded `data:` image may claim at most this many pixels (16 MP).
pub const SVG_MAX_EMBEDDED_PIXELS: u64 = 16_000_000;
/// All embedded `data:` images in one document may claim at most this many
/// pixels (64 MP). Counted from headers while parsing, before resvg decodes.
pub const SVG_MAX_EMBEDDED_PIXELS_TOTAL: u64 = 64_000_000;
/// XML element nesting cap. Checked by a linear pre-scan, then again by
/// `usvg::Options::max_depth`, so a deep tree never reaches recursive parse
/// on the loader thread (Rust's default stack is 2 MiB).
pub const SVG_MAX_NESTING: u32 = 256;
/// If usvg parse alone exceeds this, skip the render. resvg cannot be
/// interrupted mid-render.
pub const SVG_PARSE_BUDGET: Duration = Duration::from_millis(1_500);
/// How many leading bytes an extensionless sniff inspects.
pub const SVG_SNIFF_BYTES: usize = 512;
/// Loader / test thread stack. Matches `std::thread` default (2 MiB).
pub const SVG_LOADER_STACK: usize = 2 * 1024 * 1024;
/// GZip magic. `.svgz` is a gzip-compressed SVG.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Default body used when the window size is not known yet: the default
/// preview outer size minus the header, at 96 DPI.
const DEFAULT_TARGET_WIDTH: u32 = 760;
const DEFAULT_TARGET_HEIGHT: u32 = 468;

/// Render target for one SVG preview: the current body size at the monitor DPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvgTarget {
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
}

impl Default for SvgTarget {
    fn default() -> Self {
        Self {
            width: DEFAULT_TARGET_WIDTH,
            height: DEFAULT_TARGET_HEIGHT,
            dpi: 96,
        }
    }
}

/// Caps and the current window body for one SVG load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvgLimits {
    pub max_file_bytes: u64,
    pub max_decompressed_bytes: u64,
    pub max_nodes: u32,
    pub max_pixels: u64,
    pub max_dimension: u32,
    pub max_display_dimension: u32,
    pub max_nesting: u32,
    pub parse_budget: Duration,
    pub target: SvgTarget,
}

impl Default for SvgLimits {
    fn default() -> Self {
        let image = ImageLimits::default();
        Self {
            max_file_bytes: SVG_MAX_FILE_BYTES,
            max_decompressed_bytes: SVGZ_MAX_DECOMPRESSED_BYTES,
            max_nodes: SVG_MAX_NODES,
            max_pixels: SVG_MAX_PIXELS.min(image.max_pixels),
            max_dimension: image.max_dimension,
            max_display_dimension: image.max_display_dimension,
            max_nesting: SVG_MAX_NESTING,
            parse_budget: SVG_PARSE_BUDGET,
            target: SvgTarget::default(),
        }
    }
}

/// Body size from a client rectangle, header subtracted, at `dpi`.
pub fn svg_target_from_client(client_w: i32, client_h: i32, dpi: u32) -> SvgTarget {
    let dpi = effective_dpi(dpi);
    let header = scaled_header_height(dpi);
    let body_h = client_h.saturating_sub(header).max(1);
    let body_w = client_w.max(1);
    SvgTarget {
        width: u32::try_from(body_w).unwrap_or(1),
        height: u32::try_from(body_h).unwrap_or(1),
        dpi,
    }
}

/// Body size from a remembered outer window, treated as the client box.
pub fn svg_target_from_opening(outer: LogicalSize, dpi: u32) -> SvgTarget {
    let dpi = effective_dpi(dpi);
    let width = crate::chrome::logical_to_physical(outer, dpi).width;
    let height = crate::chrome::logical_to_physical(outer, dpi).height;
    svg_target_from_client(width, height, dpi)
}

/// Why an SVG was not previewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgError {
    FileTooLarge { bytes: u64 },
    InflateTooLarge,
    TooManyNodes { count: u32 },
    TooDeep,
    InvalidSize,
    ParseTooSlow,
    Malformed,
    Cancelled,
}

impl std::fmt::Display for SvgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SvgError::FileTooLarge { bytes } => {
                write!(f, "svg file too large ({bytes} bytes)")
            }
            SvgError::InflateTooLarge => write!(f, "svgz decompressed too large"),
            SvgError::TooManyNodes { count } => write!(f, "svg too many nodes ({count})"),
            SvgError::TooDeep => write!(f, "svg too deeply nested"),
            SvgError::InvalidSize => write!(f, "svg has an invalid size"),
            SvgError::ParseTooSlow => write!(f, "svg parse exceeded budget"),
            SvgError::Malformed => write!(f, "malformed svg"),
            SvgError::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl SvgError {
    /// Sentence after `{name} can't be previewed —`.
    pub fn reason(&self) -> String {
        match self {
            SvgError::FileTooLarge { bytes } => {
                format!("this SVG is too large ({})", format_compact_size(*bytes))
            }
            SvgError::InflateTooLarge => "this compressed SVG is too large".to_string(),
            SvgError::TooManyNodes { .. } => "this SVG is too complex".to_string(),
            SvgError::TooDeep => "this SVG is too deeply nested".to_string(),
            SvgError::InvalidSize => "this SVG has an invalid size".to_string(),
            SvgError::ParseTooSlow => "this SVG took too long to parse".to_string(),
            SvgError::Malformed => "this SVG looks damaged or isn't valid SVG".to_string(),
            SvgError::Cancelled => "preview cancelled".to_string(),
        }
    }

    pub fn user_message(&self, file_name: &str) -> String {
        let name = if file_name.is_empty() {
            "This file"
        } else {
            file_name
        };
        format!("{name} can't be previewed — {}", self.reason())
    }

    pub fn header_status(&self) -> String {
        match self {
            SvgError::FileTooLarge { .. } | SvgError::InflateTooLarge => "Too large".to_string(),
            SvgError::TooManyNodes { .. } | SvgError::TooDeep | SvgError::ParseTooSlow => {
                "Too complex".to_string()
            }
            SvgError::InvalidSize => "Invalid size".to_string(),
            SvgError::Malformed => "Damaged".to_string(),
            SvgError::Cancelled => "Cancelled".to_string(),
        }
    }
}

/// True for `.svg` and `.svgz`, any case. A file named `.svg` / `.svgz` counts.
pub fn is_svg_path(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        return ext.eq_ignore_ascii_case("svg") || ext.eq_ignore_ascii_case("svgz");
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(".svg") || name.eq_ignore_ascii_case(".svgz"))
}

/// True when `header` looks like SVG or gzip-compressed SVG.
///
/// A UTF-8 BOM is skipped. `<svg` at the start matches. `<?xml` matches only
/// when `<svg` also appears later in the same window. GZip magic (`1f 8b`)
/// matches as SVGZ. This is a cheap prefix check for extensionless files.
pub fn sniff_svg(header: &[u8]) -> bool {
    if header.starts_with(&GZIP_MAGIC) {
        return true;
    }
    looks_like_svg_xml(header)
}

fn looks_like_svg_xml(header: &[u8]) -> bool {
    let bytes = strip_utf8_bom(header);
    let bytes = skip_ascii_ws(bytes);
    if starts_with_svg_tag(bytes) {
        return true;
    }
    if starts_with_ignore_ascii(bytes, b"<?xml") {
        return find_svg_tag(bytes);
    }
    false
}

fn strip_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

fn skip_ascii_ws(bytes: &[u8]) -> &[u8] {
    let n = bytes
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        .unwrap_or(bytes.len());
    &bytes[n..]
}

fn starts_with_ignore_ascii(hay: &[u8], needle: &[u8]) -> bool {
    hay.len() >= needle.len() && hay[..needle.len()].eq_ignore_ascii_case(needle)
}

fn starts_with_svg_tag(bytes: &[u8]) -> bool {
    if !starts_with_ignore_ascii(bytes, b"<svg") {
        return false;
    }
    match bytes.get(4) {
        None | Some(b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/') => true,
        Some(_) => false,
    }
}

fn find_svg_tag(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' && starts_with_svg_tag(&bytes[i..]) {
            return true;
        }
        i += 1;
    }
    false
}

/// Cheap scan for a `<text` / `<tspan` tag. False positives only load fonts.
pub fn svg_contains_text(bytes: &[u8]) -> bool {
    let bytes = strip_utf8_bom(bytes);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &bytes[i + 1..];
            if starts_with_ignore_ascii(rest, b"text") || tag_named(rest, b"tspan") {
                return true;
            }
        }
        i += 1;
    }
    false
}

fn tag_named(after_lt: &[u8], name: &[u8]) -> bool {
    if after_lt.len() < name.len() {
        return false;
    }
    if !after_lt[..name.len()].eq_ignore_ascii_case(name) {
        return false;
    }
    matches!(
        after_lt.get(name.len()),
        None | Some(b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
    )
}

/// Linear XML element-nesting depth. Comments, PI, CDATA, and DOCTYPE do not
/// count. Self-closing tags do not increase depth. Used to reject a deep tree
/// before roxmltree or usvg recurse.
pub fn xml_element_depth(bytes: &[u8]) -> u32 {
    let s = strip_utf8_bom(bytes);
    let mut i = 0;
    let mut depth = 0u32;
    let mut max = 0u32;
    while i < s.len() {
        if s[i] != b'<' {
            i += 1;
            continue;
        }
        if skip_delimited(s, &mut i, b"<!--", b"-->")
            || skip_delimited(s, &mut i, b"<![CDATA[", b"]]>")
            || skip_delimited(s, &mut i, b"<?", b"?>")
        {
            continue;
        }
        if starts_with_ignore_ascii(&s[i..], b"<!DOCTYPE")
            || starts_with_ignore_ascii(&s[i..], b"<!doctype")
        {
            skip_until(s, &mut i, b'>');
            continue;
        }
        if i + 1 < s.len() && s[i + 1] == b'/' {
            depth = depth.saturating_sub(1);
            skip_until(s, &mut i, b'>');
            continue;
        }
        i += 1;
        let mut self_close = false;
        while i < s.len() && s[i] != b'>' {
            if s[i] == b'/' && i + 1 < s.len() && s[i + 1] == b'>' {
                self_close = true;
            }
            i += 1;
        }
        if i < s.len() {
            i += 1;
        }
        if !self_close {
            depth = depth.saturating_add(1);
            max = max.max(depth);
        }
    }
    max
}

fn skip_delimited(s: &[u8], i: &mut usize, open: &[u8], close: &[u8]) -> bool {
    if !s[*i..].starts_with(open) {
        return false;
    }
    *i += open.len();
    if let Some(rel) = find_bytes(&s[*i..], close) {
        *i += rel + close.len();
    } else {
        *i = s.len();
    }
    true
}

fn skip_until(s: &[u8], i: &mut usize, end: u8) {
    while *i < s.len() && s[*i] != end {
        *i += 1;
    }
    if *i < s.len() {
        *i += 1;
    }
}

fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Inflate gzip, stopping once `max` decompressed bytes would be exceeded.
pub fn inflate_svgz(bytes: &[u8], max: u64) -> Result<Vec<u8>, SvgError> {
    let mut decoder = GzDecoder::new(bytes);
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = match decoder.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(_) => return Err(SvgError::Malformed),
        };
        let next = (out.len() as u64).saturating_add(n as u64);
        if next > max {
            return Err(SvgError::InflateTooLarge);
        }
        out.extend_from_slice(&buf[..n]);
    }
    if out.is_empty() {
        return Err(SvgError::Malformed);
    }
    Ok(out)
}

/// Source pixels (actual size at DPI) and the pixmap we will allocate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvgOutputSize {
    pub source_width: u32,
    pub source_height: u32,
    pub render_width: u32,
    pub render_height: u32,
}

/// Clamp a parsed SVG size to the pixel cap and fit it into the target body.
///
/// Zero, negative, and non-finite sizes are [`SvgError::InvalidSize`]. A huge
/// `viewBox` or `width="1e9"` is scaled down; it never allocates a giant
/// buffer.
pub fn svg_output_size(
    intrinsic_w: f32,
    intrinsic_h: f32,
    limits: &SvgLimits,
) -> Result<SvgOutputSize, SvgError> {
    if !intrinsic_w.is_finite()
        || !intrinsic_h.is_finite()
        || intrinsic_w <= 0.0
        || intrinsic_h <= 0.0
    {
        return Err(SvgError::InvalidSize);
    }
    let max_dim = f64::from(limits.max_dimension.max(1));
    let max_pixels = limits.max_pixels.max(1) as f64;
    let w = f64::from(intrinsic_w);
    let h = f64::from(intrinsic_h);
    let mut scale = 1.0_f64;
    if w > max_dim {
        scale = scale.min(max_dim / w);
    }
    if h > max_dim {
        scale = scale.min(max_dim / h);
    }
    let pixels = w * h;
    if pixels * scale * scale > max_pixels {
        scale = scale.min((max_pixels / pixels).sqrt());
    }
    let src_w = clamp_side(w * scale);
    let src_h = clamp_side(h * scale);
    if src_w == 0 || src_h == 0 {
        return Err(SvgError::InvalidSize);
    }

    let box_w = limits
        .target
        .width
        .max(1)
        .min(limits.max_display_dimension.max(1));
    let box_h = limits
        .target
        .height
        .max(1)
        .min(limits.max_display_dimension.max(1));
    let (mut rw, mut rh) = fit_size(src_w, src_h, box_w, box_h);
    if rw == 0 || rh == 0 {
        return Err(SvgError::InvalidSize);
    }
    let out_pixels = u64::from(rw).saturating_mul(u64::from(rh));
    if out_pixels > limits.max_pixels {
        let scale = (limits.max_pixels as f64 / out_pixels as f64).sqrt();
        rw = ((f64::from(rw) * scale).floor() as u32).max(1);
        rh = ((f64::from(rh) * scale).floor() as u32).max(1);
    }
    Ok(SvgOutputSize {
        source_width: src_w,
        source_height: src_h,
        render_width: rw,
        render_height: rh,
    })
}

fn clamp_side(v: f64) -> u32 {
    if !v.is_finite() || v <= 0.0 {
        return 0;
    }
    v.floor().min(f64::from(u32::MAX)) as u32
}

/// Bytes ready for usvg: UTF-8 SVG, already inflated, already depth-checked.
struct SvgSource {
    xml: String,
    wants_text: bool,
}

fn prepare_source(bytes: &[u8], gzip: bool, limits: &SvgLimits) -> Result<SvgSource, SvgError> {
    // Inflate at most once. `Tree::from_data` would inflate gzip magic again
    // with no size cap, including a `.svg` that is actually gzip and a
    // `.svgz` whose payload is gzip a second time.
    let xml_bytes = if gzip || bytes.starts_with(&GZIP_MAGIC) {
        let inflated = inflate_svgz(bytes, limits.max_decompressed_bytes)?;
        if inflated.starts_with(&GZIP_MAGIC) {
            return Err(SvgError::Malformed);
        }
        inflated
    } else {
        bytes.to_vec()
    };
    let xml_bytes = strip_utf8_bom(&xml_bytes).to_vec();
    let xml = String::from_utf8(xml_bytes).map_err(|_| SvgError::Malformed)?;
    if xml_element_depth(xml.as_bytes()) > limits.max_nesting {
        return Err(SvgError::TooDeep);
    }
    let wants_text = svg_contains_text(xml.as_bytes());
    Ok(SvgSource { xml, wants_text })
}

/// Process-wide system font cache. Loaded once, on the worker, and only when
/// an SVG actually contains text.
static FONT_DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
static FONT_LOAD_TOOK: OnceLock<Duration> = OnceLock::new();

/// Load system fonts if this is the first text SVG. Safe to call from the
/// loader thread. SVGs without text never call this.
pub fn ensure_system_fonts() -> Arc<usvg::fontdb::Database> {
    FONT_DB
        .get_or_init(|| {
            let start = Instant::now();
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            let _ = FONT_LOAD_TOOK.set(start.elapsed());
            Arc::new(db)
        })
        .clone()
}

/// `Some` after the first [`ensure_system_fonts`] call.
pub fn system_font_load_time() -> Option<Duration> {
    FONT_LOAD_TOOK.get().copied()
}

/// True when fontdb has been loaded in this process.
pub fn system_fonts_loaded() -> bool {
    FONT_DB.get().is_some()
}

/// Serialize SVG parse+render so stale jobs cannot pile up. resvg cannot be
/// interrupted mid-render; a cancelled job waiting for the lock polls and
/// exits instead of rendering. Limit: one in-flight render may still run to
/// completion after it has been cancelled.
static RENDER_LOCK: Mutex<()> = Mutex::new(());

fn with_render_slot<T>(
    cancel: &CancelToken,
    gen: Generation,
    f: impl FnOnce() -> Result<T, SvgError>,
) -> Result<T, SvgError> {
    loop {
        if cancel.is_cancelled(&gen) {
            return Err(SvgError::Cancelled);
        }
        match RENDER_LOCK.try_lock() {
            Ok(_guard) => {
                if cancel.is_cancelled(&gen) {
                    return Err(SvgError::Cancelled);
                }
                return f();
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(std::sync::TryLockError::Poisoned(poison)) => {
                drop(poison.into_inner());
            }
        }
    }
}

fn svg_options(
    dpi: f32,
    fonts: Option<Arc<usvg::fontdb::Database>>,
    max_depth: u32,
) -> Options<'static> {
    // One counter per parse. resvg calls the resolver while building the tree.
    let embedded_pixels = Arc::new(AtomicU64::new(0));
    let mut opt = Options {
        resources_dir: None,
        image_href_resolver: ImageHrefResolver {
            resolve_data: Box::new(move |mime, data, _| {
                resolve_embedded_image(mime, data, &embedded_pixels)
            }),
            resolve_string: Box::new(|_, _| None),
        },
        max_depth,
        ..Options::default()
    };
    opt.dpi = dpi;
    if let Some(db) = fonts {
        opt.fontdb = db;
    }
    opt
}

/// Only `data:` raster images whose header fits the pixel caps.
///
/// Nested SVG and anything else is dropped. A header that cannot be read is
/// dropped. Pixel dimensions come from the container header, not a decode:
/// resvg's raster path allocates the pixmap from that header at draw time.
fn resolve_embedded_image(
    mime: &str,
    data: Arc<Vec<u8>>,
    embedded_pixels: &AtomicU64,
) -> Option<ImageKind> {
    if data.len() as u64 > SVG_MAX_EMBEDDED_BYTES {
        return None;
    }
    let mime = mime.to_ascii_lowercase();
    let format = embedded_image_format(&mime)?;
    let pixels = embedded_raster_pixels(format, data.as_slice())?;
    if pixels > SVG_MAX_EMBEDDED_PIXELS || !reserve_embedded_pixels(embedded_pixels, pixels) {
        return None;
    }
    Some(match format {
        image::ImageFormat::Png => ImageKind::PNG(data),
        image::ImageFormat::Jpeg => ImageKind::JPEG(data),
        image::ImageFormat::Gif => ImageKind::GIF(data),
        image::ImageFormat::WebP => ImageKind::WEBP(data),
        _ => return None,
    })
}

fn embedded_image_format(mime: &str) -> Option<image::ImageFormat> {
    match mime {
        "image/png" => Some(image::ImageFormat::Png),
        "image/jpeg" | "image/jpg" => Some(image::ImageFormat::Jpeg),
        "image/gif" => Some(image::ImageFormat::Gif),
        "image/webp" => Some(image::ImageFormat::WebP),
        _ => None,
    }
}

/// Header width × height, or `None` when the header cannot be read.
///
/// [`image::ImageReader::into_dimensions`] stops once the container header is
/// known. The side cap is the per-image pixel cap, so a `1×N` image still
/// fits and a wider claim is refused before a scanline is sized.
fn embedded_raster_pixels(format: image::ImageFormat, data: &[u8]) -> Option<u64> {
    let mut reader = image::ImageReader::with_format(Cursor::new(data), format);
    let mut limits = image::Limits::default();
    let max_side = u32::try_from(SVG_MAX_EMBEDDED_PIXELS).unwrap_or(u32::MAX);
    limits.max_image_width = Some(max_side);
    limits.max_image_height = Some(max_side);
    reader.limits(limits);
    let (width, height) = reader.into_dimensions().ok()?;
    if width == 0 || height == 0 {
        return None;
    }
    u64::from(width).checked_mul(u64::from(height))
}

/// Keep `pixels` when the document total would still be within the cap.
fn reserve_embedded_pixels(budget: &AtomicU64, pixels: u64) -> bool {
    budget
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            let next = current.checked_add(pixels)?;
            if next > SVG_MAX_EMBEDDED_PIXELS_TOTAL {
                None
            } else {
                Some(next)
            }
        })
        .is_ok()
}

fn map_usvg_error(err: usvg::Error) -> SvgError {
    match err {
        usvg::Error::InvalidSize => SvgError::InvalidSize,
        usvg::Error::ElementsLimitReached => SvgError::TooManyNodes {
            count: SVG_MAX_NODES,
        },
        _ => SvgError::Malformed,
    }
}

fn count_nodes(tree: &Tree) -> u32 {
    let mut n = 0u32;
    let mut stack: Vec<&usvg::Group> = vec![tree.root()];
    while let Some(group) = stack.pop() {
        n = n.saturating_add(1);
        for child in group.children() {
            match child {
                usvg::Node::Group(g) => stack.push(g),
                _ => n = n.saturating_add(1),
            }
        }
    }
    n
}

/// Parse with usvg. Fonts are loaded only when `source.wants_text`.
pub fn parse_svg_tree(
    source: &str,
    wants_text: bool,
    limits: &SvgLimits,
) -> Result<(Tree, Duration), SvgError> {
    let fonts = if wants_text {
        Some(ensure_system_fonts())
    } else {
        None
    };
    let opt = svg_options(
        effective_dpi(limits.target.dpi) as f32,
        fonts,
        limits.max_nesting,
    );
    let start = Instant::now();
    // `from_str` does not inflate gzip. Compressed input is rejected in
    // `prepare_source` so `Tree::from_data` is never given those bytes.
    let tree = Tree::from_str(source, &opt).map_err(map_usvg_error)?;
    let parse_took = start.elapsed();
    let nodes = count_nodes(&tree);
    if nodes > limits.max_nodes {
        return Err(SvgError::TooManyNodes { count: nodes });
    }
    Ok((tree, parse_took))
}

fn rasterize_tree(tree: &Tree, limits: &SvgLimits) -> Result<DecodedImage, SvgError> {
    let size = tree.size();
    let out = svg_output_size(size.width(), size.height(), limits)?;
    let mut pixmap =
        Pixmap::new(out.render_width, out.render_height).ok_or(SvgError::InvalidSize)?;
    pixmap.fill(Color::WHITE);
    let sx = out.render_width as f32 / size.width();
    let sy = out.render_height as f32 / size.height();
    let transform = Transform::from_scale(sx, sy);
    resvg::render(tree, transform, &mut pixmap.as_mut());
    let bgra = pixmap_to_bgra_on_white(&pixmap);
    Ok(DecodedImage {
        width: out.render_width,
        height: out.render_height,
        source_width: out.source_width,
        source_height: out.source_height,
        format: ImageFormat::Svg,
        bgra,
    })
}

/// Premultiplied RGBA (tiny-skia) onto white, written B, G, R, 255.
fn pixmap_to_bgra_on_white(pixmap: &Pixmap) -> Vec<u8> {
    let src = pixmap.data();
    let mut bgra = Vec::with_capacity(src.len());
    for px in src.chunks_exact(4) {
        let r = px[0];
        let g = px[1];
        let b = px[2];
        let a = px[3];
        if a == 255 {
            bgra.push(b);
            bgra.push(g);
            bgra.push(r);
            bgra.push(255);
        } else {
            let inv = 255 - a;
            bgra.push(b.saturating_add(inv));
            bgra.push(g.saturating_add(inv));
            bgra.push(r.saturating_add(inv));
            bgra.push(255);
        }
    }
    bgra
}

/// Parse and render `bytes`. `gzip` forces inflate even without a magic prefix.
///
/// Cancellation is checked before taking the render slot, after parse, and
/// before rasterize. A stale result is discarded. One render that already
/// started cannot be interrupted (resvg has no cancel hook).
pub fn render_svg(
    bytes: &[u8],
    gzip: bool,
    limits: &SvgLimits,
    cancel: &CancelToken,
    gen: Generation,
) -> Result<DecodedImage, SvgError> {
    if cancel.is_cancelled(&gen) {
        return Err(SvgError::Cancelled);
    }
    let source = prepare_source(bytes, gzip, limits)?;
    if cancel.is_cancelled(&gen) {
        return Err(SvgError::Cancelled);
    }
    with_render_slot(cancel, gen, || {
        let (tree, parse_took) = parse_svg_tree(&source.xml, source.wants_text, limits)?;
        if cancel.is_cancelled(&gen) {
            return Err(SvgError::Cancelled);
        }
        if parse_took > limits.parse_budget {
            return Err(SvgError::ParseTooSlow);
        }
        rasterize_tree(&tree, limits)
    })
}

/// Parse, then refuse to rasterize when `gen` is already cancelled.
///
/// Used to test the parse/render seam without racing the render lock.
pub fn rasterize_parsed_svg(
    tree: &Tree,
    limits: &SvgLimits,
    cancel: &CancelToken,
    gen: Generation,
) -> Result<DecodedImage, SvgError> {
    if cancel.is_cancelled(&gen) {
        return Err(SvgError::Cancelled);
    }
    rasterize_tree(tree, limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};
    use std::io::Write;
    use std::path::PathBuf;

    fn limits() -> SvgLimits {
        SvgLimits::default()
    }

    fn live() -> (CancelToken, Generation) {
        let token = CancelToken::new();
        let gen = token.generate();
        (token, gen)
    }

    fn render(xml: &str) -> Result<DecodedImage, SvgError> {
        let (token, gen) = live();
        render_svg(xml.as_bytes(), false, &limits(), &token, gen)
    }

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    fn pixel(img: &DecodedImage, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        img.bgra[i..i + 4].try_into().unwrap()
    }

    fn b64(data: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let a = chunk[0];
            let b = chunk.get(1).copied().unwrap_or(0);
            let c = chunk.get(2).copied().unwrap_or(0);
            out.push(T[(a >> 2) as usize] as char);
            out.push(T[(((a & 3) << 4) | (b >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(T[(((b & 15) << 2) | (c >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(T[(c & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    fn rgba_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        PngEncoder::new(&mut buf)
            .write_image(rgba, width, height, ExtendedColorType::Rgba8)
            .unwrap();
        buf
    }

    fn blue_rect() -> &'static str {
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="blue"/></svg>"#
    }

    #[test]
    fn default_caps_are_the_product_budget() {
        let limits = SvgLimits::default();
        assert_eq!(limits.max_file_bytes, 8 * 1024 * 1024);
        assert_eq!(limits.max_decompressed_bytes, 16 * 1024 * 1024);
        assert_eq!(limits.max_nodes, 200_000);
        assert_eq!(limits.max_pixels, 32_000_000);
        assert_eq!(SVG_MAX_EMBEDDED_PIXELS, 16_000_000);
        assert_eq!(SVG_MAX_EMBEDDED_PIXELS_TOTAL, 64_000_000);
        assert_eq!(limits.max_dimension, 30_000);
        assert_eq!(limits.max_display_dimension, 4096);
        assert_eq!(limits.max_nesting, 256);
        assert_eq!(limits.parse_budget, Duration::from_millis(1_500));
        assert_eq!(limits.target.width, 760);
        assert_eq!(limits.target.height, 468);
        assert_eq!(limits.target.dpi, 96);
        assert!(limits.max_pixels <= ImageLimits::default().max_pixels);
    }

    #[test]
    fn svg_path_is_case_insensitive() {
        assert!(is_svg_path(Path::new("icon.svg")));
        assert!(is_svg_path(Path::new("icon.SVG")));
        assert!(is_svg_path(Path::new("icon.svgz")));
        assert!(is_svg_path(Path::new("icon.SVGZ")));
        assert!(is_svg_path(Path::new(".svg")));
        assert!(is_svg_path(Path::new(".SVGZ")));
        assert!(!is_svg_path(Path::new("icon.png")));
        assert!(!is_svg_path(Path::new("icon.md")));
        assert!(!is_svg_path(Path::new("svg")));
        assert!(!is_svg_path(Path::new("notes.txt")));
        let nested = PathBuf::from("docs").join("Logo.Svg");
        assert!(is_svg_path(&nested));
    }

    #[test]
    fn sniff_svg_matches_tag_xml_and_gzip() {
        assert!(sniff_svg(b"<svg xmlns='http://www.w3.org/2000/svg'>"));
        assert!(sniff_svg(b"<SVG width='1'>"));
        assert!(sniff_svg(b"  \n<svg>"));
        assert!(sniff_svg(b"<?xml version=\"1.0\"?><svg>"));
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(b"<svg>");
        assert!(sniff_svg(&bom));
        assert!(sniff_svg(&[0x1f, 0x8b, 0x08]));
        assert!(!sniff_svg(b"<html>"));
        assert!(!sniff_svg(b"<?xml version=\"1.0\"?><root/>"));
        assert!(!sniff_svg(b"not svg"));
        assert!(!sniff_svg(b""));
        assert!(!sniff_svg(b"<svgsome>"));
        assert!(!sniff_svg(b"note <svg"));
    }

    #[test]
    fn text_scan_finds_text_and_tspan_only() {
        assert!(svg_contains_text(b"<svg><text>Hi</text></svg>"));
        assert!(svg_contains_text(b"<svg><TEXT x='0'>Hi</TEXT></svg>"));
        assert!(svg_contains_text(b"<svg><tspan>Hi</tspan></svg>"));
        assert!(svg_contains_text(b"<svg><textPath>Hi</textPath></svg>"));
        assert!(!svg_contains_text(b"<svg><rect/></svg>"));
        assert!(!svg_contains_text(b"<svg><path id='text-shape'/></svg>"));
    }

    #[test]
    fn nesting_depth_counts_elements_not_self_closing() {
        assert_eq!(xml_element_depth(b"<svg/>"), 0);
        assert_eq!(xml_element_depth(b"<svg></svg>"), 1);
        assert_eq!(xml_element_depth(b"<svg><g><g></g></g></svg>"), 3);
        assert_eq!(xml_element_depth(b"<svg><rect/><rect/></svg>"), 1);
        let comment = b"<svg><!-- <g><g><g> --><rect/></svg>";
        assert_eq!(xml_element_depth(comment), 1);
        let mut nested = String::from("<svg>");
        for _ in 0..10 {
            nested.push_str("<g>");
        }
        for _ in 0..10 {
            nested.push_str("</g>");
        }
        nested.push_str("</svg>");
        assert_eq!(xml_element_depth(nested.as_bytes()), 11);
    }

    #[test]
    fn output_size_rejects_zero_negative_nan() {
        let limits = limits();
        assert_eq!(
            svg_output_size(0.0, 10.0, &limits),
            Err(SvgError::InvalidSize)
        );
        assert_eq!(
            svg_output_size(10.0, 0.0, &limits),
            Err(SvgError::InvalidSize)
        );
        assert_eq!(
            svg_output_size(-4.0, 10.0, &limits),
            Err(SvgError::InvalidSize)
        );
        assert_eq!(
            svg_output_size(f32::NAN, 10.0, &limits),
            Err(SvgError::InvalidSize)
        );
        assert_eq!(
            svg_output_size(10.0, f32::INFINITY, &limits),
            Err(SvgError::InvalidSize)
        );
    }

    #[test]
    fn huge_viewbox_is_clamped_to_the_pixel_cap() {
        let limits = limits();
        let out = svg_output_size(1.0e9, 1.0e9, &limits).unwrap();
        let pixels = u64::from(out.render_width) * u64::from(out.render_height);
        assert!(pixels <= limits.max_pixels, "{pixels}");
        assert!(out.render_width <= limits.max_display_dimension);
        assert!(out.render_height <= limits.max_display_dimension);
        assert!(out.source_width <= limits.max_dimension);
        assert!(out.source_height <= limits.max_dimension);
        let src_pixels = u64::from(out.source_width) * u64::from(out.source_height);
        assert!(src_pixels <= limits.max_pixels, "{src_pixels}");
        assert!(out.render_width > 0 && out.render_height > 0);
    }

    #[test]
    fn small_svg_is_not_upscaled_to_the_window() {
        let mut limits = limits();
        limits.target = SvgTarget {
            width: 800,
            height: 600,
            dpi: 96,
        };
        let out = svg_output_size(24.0, 24.0, &limits).unwrap();
        assert_eq!((out.source_width, out.source_height), (24, 24));
        assert_eq!((out.render_width, out.render_height), (24, 24));
    }

    #[test]
    fn render_fits_a_large_svg_into_the_target() {
        let mut limits = limits();
        limits.target = SvgTarget {
            width: 80,
            height: 60,
            dpi: 96,
        };
        let out = svg_output_size(800.0, 600.0, &limits).unwrap();
        assert_eq!((out.source_width, out.source_height), (800, 600));
        assert_eq!((out.render_width, out.render_height), (80, 60));
    }

    #[test]
    fn simple_rect_renders_blue_bgra() {
        let img = render(blue_rect()).unwrap();
        assert_eq!(img.format, ImageFormat::Svg);
        assert_eq!((img.source_width, img.source_height), (8, 8));
        assert_eq!((img.width, img.height), (8, 8));
        let px = pixel(&img, 4, 4);
        assert!(px[0] > 200, "blue {}", px[0]);
        assert!(px[1] < 40, "green {}", px[1]);
        assert!(px[2] < 40, "red {}", px[2]);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn svgz_inflates_and_renders() {
        let gz = gzip(blue_rect().as_bytes());
        assert!(gz.starts_with(&GZIP_MAGIC));
        let (token, gen) = live();
        let img = render_svg(&gz, true, &limits(), &token, gen).unwrap();
        assert_eq!(img.format, ImageFormat::Svg);
        let px = pixel(&img, 4, 4);
        assert!(px[0] > 200, "blue {}", px[0]);
    }

    #[test]
    fn inflate_bomb_is_stopped_at_the_cap() {
        let zeros = vec![0u8; 8 * 1024];
        let gz = gzip(&zeros);
        assert!(
            gz.len() < 1024,
            "gzip of zeros should be small, got {}",
            gz.len()
        );
        let err = inflate_svgz(&gz, 1024).unwrap_err();
        assert_eq!(err, SvgError::InflateTooLarge);
        let ok = inflate_svgz(&gz, 16 * 1024).unwrap();
        assert_eq!(ok.len(), zeros.len());
    }

    #[test]
    fn node_count_cap_rejects_after_parse() {
        let mut xml =
            String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">"#);
        for _ in 0..12 {
            xml.push_str(r#"<rect width="1" height="1"/>"#);
        }
        xml.push_str("</svg>");
        let mut limits = limits();
        limits.max_nodes = 8;
        let (token, gen) = live();
        let err = render_svg(xml.as_bytes(), false, &limits, &token, gen).unwrap_err();
        assert!(
            matches!(err, SvgError::TooManyNodes { count } if count > 8),
            "{err:?}"
        );
    }

    #[test]
    fn zero_size_svg_is_an_error() {
        let xml = r#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="10"><rect width="1" height="1"/></svg>"#;
        assert_eq!(render(xml), Err(SvgError::InvalidSize));
        let xml = r#"<svg xmlns="http://www.w3.org/2000/svg" width="-4" height="10"><rect width="1" height="1"/></svg>"#;
        assert!(matches!(
            render(xml),
            Err(SvgError::InvalidSize | SvgError::Malformed)
        ));
    }

    #[test]
    fn huge_declared_size_does_not_allocate_a_giant_buffer() {
        let xml = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1e9" height="1e9"><rect width="1e9" height="1e9" fill="red"/></svg>"#;
        let img = render(xml).unwrap();
        let pixels = u64::from(img.width) * u64::from(img.height);
        assert!(pixels <= SVG_MAX_PIXELS, "{pixels}");
        assert!(img.width <= 4096 && img.height <= 4096);
        assert!(img.bgra.len() as u64 == pixels * 4);
    }

    #[test]
    fn malformed_xml_is_an_error_not_a_panic() {
        assert_eq!(render("<svg><"), Err(SvgError::Malformed));
        assert_eq!(render("not xml at all"), Err(SvgError::Malformed));
        assert_eq!(render(""), Err(SvgError::Malformed));
        assert_eq!(render("<html></html>"), Err(SvgError::Malformed));
    }

    #[test]
    fn script_is_ignored() {
        let xml = r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <rect id="r" width="8" height="8" fill="lime"/>
  <script type="application/ecmascript">document.getElementById('r').setAttribute('fill','#ff0000');</script>
</svg>"#;
        let img = render(xml).unwrap();
        let px = pixel(&img, 4, 4);
        assert!(px[1] > 200, "green should remain, got {px:?}");
        assert!(px[2] < 40, "script must not paint red, got {px:?}");
    }

    #[test]
    fn billion_laughs_fails_fast() {
        let xml = r#"<?xml version="1.0"?>
<!DOCTYPE lolz [
  <!ENTITY lol "lol">
  <!ENTITY lol1 "&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;&lol;">
  <!ENTITY lol2 "&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;&lol1;">
  <!ENTITY lol3 "&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;&lol2;">
  <!ENTITY lol4 "&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;&lol3;">
  <!ENTITY lol5 "&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;&lol4;">
  <!ENTITY lol6 "&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;&lol5;">
  <!ENTITY lol7 "&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;&lol6;">
  <!ENTITY lol8 "&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;&lol7;">
  <!ENTITY lol9 "&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;&lol8;">
]>
<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">&lol9;</svg>"#;
        let start = Instant::now();
        let err = render(xml).unwrap_err();
        let elapsed = start.elapsed();
        assert_eq!(err, SvgError::Malformed);
        assert!(
            elapsed < Duration::from_secs(2),
            "billion laughs should fail fast, took {elapsed:?}"
        );
    }

    #[test]
    fn deep_nesting_is_rejected_on_a_loader_sized_stack() {
        let mut xml =
            String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">"#);
        for _ in 0..100_000 {
            xml.push_str("<g>");
        }
        for _ in 0..100_000 {
            xml.push_str("</g>");
        }
        xml.push_str("</svg>");
        assert!(xml_element_depth(xml.as_bytes()) > SVG_MAX_NESTING);

        let xml = Arc::new(xml);
        let job = xml.clone();
        let handle = std::thread::Builder::new()
            .name("svg-deep-nest".to_string())
            .stack_size(SVG_LOADER_STACK)
            .spawn(move || {
                let (token, gen) = live();
                render_svg(job.as_bytes(), false, &limits(), &token, gen)
            })
            .unwrap();
        let err = handle
            .join()
            .expect("loader-sized thread must not overflow");
        assert_eq!(err, Err(SvgError::TooDeep));
    }

    #[test]
    fn modest_nesting_still_renders() {
        let mut xml =
            String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">"#);
        for _ in 0..20 {
            xml.push_str("<g>");
        }
        xml.push_str(r#"<rect width="8" height="8" fill="blue"/>"#);
        for _ in 0..20 {
            xml.push_str("</g>");
        }
        xml.push_str("</svg>");
        let img = render(&xml).unwrap();
        let px = pixel(&img, 4, 4);
        assert!(px[0] > 200, "{px:?}");
    }

    #[test]
    fn external_file_and_http_images_are_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let png = rgba_png(
            2,
            2,
            &[
                255, 0, 255, 255, 255, 0, 255, 255, 255, 0, 255, 255, 255, 0, 255, 255,
            ],
        );
        let img_path = dir.path().join("magenta.png");
        std::fs::write(&img_path, &png).unwrap();
        let relative = "magenta.png";
        let file_url = format!("file://{}", img_path.display());
        let xml = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="8" height="8">
  <rect width="8" height="8" fill="blue"/>
  <image href="{relative}" width="8" height="8"/>
  <image href="{file_url}" width="8" height="8"/>
  <image href="http://127.0.0.1:1/magenta.png" width="8" height="8"/>
  <image xlink:href="{relative}" width="8" height="8"/>
</svg>"#
        );
        let svg_path = dir.path().join("icon.svg");
        std::fs::write(&svg_path, &xml).unwrap();
        let bytes = std::fs::read(&svg_path).unwrap();
        let img = {
            let (token, gen) = live();
            render_svg(&bytes, false, &limits(), &token, gen).unwrap()
        };
        for y in 0..img.height {
            for x in 0..img.width {
                let px = pixel(&img, x, y);
                assert!(
                    px[2] < 200 || px[1] > 40,
                    "magenta from disk/http must not appear at {x},{y}: {px:?}"
                );
            }
        }
        let cx = pixel(&img, img.width / 2, img.height / 2);
        assert!(cx[0] > 200, "blue fallback {cx:?}");
    }

    #[test]
    fn data_uri_png_is_drawn() {
        let png = rgba_png(
            2,
            2,
            &[
                255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
            ],
        );
        let uri = format!("data:image/png;base64,{}", b64(&png));
        let xml = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <image href="{uri}" width="8" height="8"/>
</svg>"#
        );
        let img = render(&xml).unwrap();
        let px = pixel(&img, 4, 4);
        assert!(px[2] > 200, "red from data URI {px:?}");
        assert!(px[1] < 40 && px[0] < 40, "{px:?}");
    }

    /// 1×1 PNG with `IHDR` width and height replaced. The CRC is rewritten so
    /// a header reader accepts the claim. The `IDAT` is still one pixel.
    fn claimed_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = rgba_png(1, 1, &[255, 0, 0, 255]);
        assert_eq!(&png[12..16], b"IHDR", "IHDR is the first chunk");
        png[16..20].copy_from_slice(&width.to_be_bytes());
        png[20..24].copy_from_slice(&height.to_be_bytes());
        let crc = png_crc(&png[12..29]);
        png[29..33].copy_from_slice(&crc.to_be_bytes());
        png
    }

    fn png_crc(data: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }

    fn svg_with_pngs(pngs: &[Vec<u8>]) -> String {
        let mut xml =
            String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">"#);
        for png in pngs {
            xml.push_str(&format!(
                r#"<image href="data:image/png;base64,{}" width="8" height="8"/>"#,
                b64(png)
            ));
        }
        xml.push_str("</svg>");
        xml
    }

    fn count_embedded_images(tree: &Tree) -> u32 {
        fn walk(group: &usvg::Group) -> u32 {
            let mut n = 0u32;
            for child in group.children() {
                match child {
                    usvg::Node::Image(_) => n = n.saturating_add(1),
                    usvg::Node::Group(inner) => n = n.saturating_add(walk(inner)),
                    _ => {}
                }
            }
            n
        }
        walk(tree.root())
    }

    fn parse_only(xml: &str) -> Tree {
        let source = prepare_source(xml.as_bytes(), false, &limits()).unwrap();
        parse_svg_tree(&source.xml, source.wants_text, &limits())
            .unwrap()
            .0
    }

    #[test]
    fn huge_embedded_png_header_is_refused_quickly() {
        let png = claimed_png(60_000, 60_000);
        let start = Instant::now();
        let pixels = embedded_raster_pixels(image::ImageFormat::Png, &png).unwrap();
        assert_eq!(pixels, u64::from(60_000) * u64::from(60_000));
        assert!(
            pixels > SVG_MAX_EMBEDDED_PIXELS,
            "the claim has to exceed the per-image cap"
        );
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "header read took {:?}",
            start.elapsed()
        );

        let xml = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <rect width="8" height="8" fill="blue"/>
  <image href="data:image/png;base64,{}" width="8" height="8"/>
</svg>"#,
            b64(&png)
        );
        let start = Instant::now();
        let img = render(&xml).unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "refusing the image took {:?}",
            start.elapsed()
        );
        assert_eq!(count_embedded_images(&parse_only(&xml)), 0);
        let px = pixel(&img, 4, 4);
        assert!(px[0] > 200, "blue background remains {px:?}");
        assert!(px[2] < 40, "huge png must not paint red {px:?}");
    }

    #[test]
    fn embedded_png_over_the_per_image_cap_is_dropped() {
        let exact = claimed_png(4_000, 4_000);
        assert_eq!(
            embedded_raster_pixels(image::ImageFormat::Png, &exact),
            Some(SVG_MAX_EMBEDDED_PIXELS)
        );
        assert_eq!(
            count_embedded_images(&parse_only(&svg_with_pngs(&[exact]))),
            1
        );

        let over = claimed_png(4_000, 4_001);
        let pixels = embedded_raster_pixels(image::ImageFormat::Png, &over).unwrap();
        assert!(pixels > SVG_MAX_EMBEDDED_PIXELS);
        assert_eq!(
            count_embedded_images(&parse_only(&svg_with_pngs(&[over]))),
            0
        );
    }

    #[test]
    fn embedded_images_stop_at_the_document_pixel_cap() {
        // 8_000_000 × 8 = 64_000_000, which fills the document cap. The ninth
        // image is a real 2×2 that would paint if the budget were ignored.
        let medium = claimed_png(4_000, 2_000);
        assert_eq!(
            embedded_raster_pixels(image::ImageFormat::Png, &medium),
            Some(8_000_000)
        );
        let small = rgba_png(
            2,
            2,
            &[
                255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
            ],
        );
        let mut pngs = vec![medium; 8];
        assert_eq!(count_embedded_images(&parse_only(&svg_with_pngs(&pngs))), 8);
        pngs.push(small);
        assert_eq!(count_embedded_images(&parse_only(&svg_with_pngs(&pngs))), 8);
        // A later document does not inherit the previous total.
        let one = claimed_png(4_000, 2_000);
        assert_eq!(
            count_embedded_images(&parse_only(&svg_with_pngs(&[one]))),
            1
        );
    }

    #[test]
    fn undecodable_embedded_image_is_dropped() {
        let xml = r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <rect width="8" height="8" fill="blue"/>
  <image href="data:image/png;base64,AAAA" width="8" height="8"/>
  <image href="data:image/png;base64," width="8" height="8"/>
</svg>"#;
        let img = render(xml).unwrap();
        let px = pixel(&img, 4, 4);
        assert!(px[0] > 200, "blue remains {px:?}");
        assert_eq!(count_embedded_images(&parse_only(xml)), 0);
    }

    #[test]
    fn gzip_svg_is_inflated_once_before_usvg() {
        let gz = gzip(blue_rect().as_bytes());
        for flag in [false, true] {
            let source = prepare_source(&gz, flag, &limits()).unwrap();
            assert_eq!(source.xml, blue_rect());
            assert!(!source.xml.as_bytes().starts_with(&GZIP_MAGIC));
            let (token, gen) = live();
            let img = render_svg(&gz, flag, &limits(), &token, gen).unwrap();
            let px = pixel(&img, 4, 4);
            assert!(px[0] > 200, "flag {flag} blue {px:?}");
        }
    }

    #[test]
    fn gzip_bomb_hits_our_inflate_cap_not_usvg() {
        let zeros = vec![0u8; (SVGZ_MAX_DECOMPRESSED_BYTES as usize) + 1024];
        let gz = gzip(&zeros);
        assert!(
            gz.len() < 64 * 1024,
            "compressed bomb should stay small, got {}",
            gz.len()
        );
        for flag in [false, true] {
            let (token, gen) = live();
            let err = render_svg(&gz, flag, &limits(), &token, gen).unwrap_err();
            assert_eq!(err, SvgError::InflateTooLarge, "flag {flag}");
        }
    }

    #[test]
    fn double_gzip_is_rejected_after_one_inflate() {
        let outer = gzip(&gzip(blue_rect().as_bytes()));
        for flag in [false, true] {
            let err = prepare_source(&outer, flag, &limits()).unwrap_err();
            assert_eq!(err, SvgError::Malformed, "flag {flag}");
            let (token, gen) = live();
            assert_eq!(
                render_svg(&outer, flag, &limits(), &token, gen),
                Err(SvgError::Malformed)
            );
        }

        // The inner payload is past the inflate cap. A second inflate would
        // either hit that cap or allocate it. One inflate yields a small gzip
        // blob, which is rejected.
        let zeros = vec![0u8; (SVGZ_MAX_DECOMPRESSED_BYTES as usize) + 1024];
        let outer = gzip(&gzip(&zeros));
        assert!(
            outer.len() < 64 * 1024,
            "double-gzip bomb should stay small, got {}",
            outer.len()
        );
        let start = Instant::now();
        let err = prepare_source(&outer, true, &limits()).unwrap_err();
        assert_eq!(err, SvgError::Malformed);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "second layer must not be inflated, took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn nested_svg_data_uri_is_not_drawn() {
        let nested = br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="red"/></svg>"#;
        let uri = format!("data:image/svg+xml;base64,{}", b64(nested));
        let xml = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">
  <rect width="8" height="8" fill="blue"/>
  <image href="{uri}" width="8" height="8"/>
</svg>"#
        );
        let img = render(&xml).unwrap();
        assert_eq!(count_embedded_images(&parse_only(&xml)), 0);
        let px = pixel(&img, 4, 4);
        assert!(px[0] > 200, "blue remains {px:?}");
        assert!(px[2] < 40, "nested svg must not paint red {px:?}");
    }

    fn assert_not_red(img: &DecodedImage) {
        for y in 0..img.height {
            for x in 0..img.width {
                let px = pixel(img, x, y);
                assert!(
                    !(px[2] > 200 && px[1] < 40 && px[0] < 40),
                    "red must not appear at {x},{y}: {px:?}"
                );
            }
        }
    }

    #[test]
    fn external_use_and_feimage_are_not_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other.svg");
        std::fs::write(
            &other,
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect id="box" width="8" height="8" fill="red"/></svg>"#,
        )
        .unwrap();
        let png = rgba_png(
            2,
            2,
            &[
                255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
            ],
        );
        let png_path = dir.path().join("red.png");
        std::fs::write(&png_path, &png).unwrap();
        let file_url = format!("file://{}", other.display());
        let png_url = format!("file://{}", png_path.display());
        let xml = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="8" height="8">
  <rect width="8" height="8" fill="blue"/>
  <use href="other.svg#box"/>
  <use href="{file_url}#box"/>
  <use xlink:href="other.svg#box"/>
</svg>"#
        );
        let img = render(&xml).unwrap();
        let px = pixel(&img, 4, 4);
        assert!(px[0] > 200, "use must leave the blue rect {px:?}");
        assert_not_red(&img);

        let xml = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="8" height="8">
  <defs>
    <filter id="f" x="0" y="0" width="8" height="8" filterUnits="userSpaceOnUse">
      <feImage href="red.png" x="0" y="0" width="8" height="8"/>
      <feImage href="{png_url}" x="0" y="0" width="8" height="8"/>
      <feImage href="http://127.0.0.1:1/red.png" x="0" y="0" width="8" height="8"/>
      <feImage xlink:href="red.png" x="0" y="0" width="8" height="8"/>
    </filter>
  </defs>
  <rect width="8" height="8" fill="blue" filter="url(#f)"/>
</svg>"#
        );
        let img = render(&xml).unwrap();
        assert_not_red(&img);
    }

    #[test]
    fn cancellation_between_parse_and_render_discards_the_result() {
        let (token, gen) = live();
        let source = prepare_source(blue_rect().as_bytes(), false, &limits()).unwrap();
        let (tree, _) = parse_svg_tree(&source.xml, source.wants_text, &limits()).unwrap();
        let stale = gen;
        let _live = token.generate();
        assert!(token.is_cancelled(&stale));
        assert_eq!(
            rasterize_parsed_svg(&tree, &limits(), &token, stale),
            Err(SvgError::Cancelled)
        );
        let (token, gen) = live();
        let _live = token.generate();
        assert_eq!(
            render_svg(blue_rect().as_bytes(), false, &limits(), &token, gen),
            Err(SvgError::Cancelled)
        );
    }

    #[test]
    fn text_svg_loads_fontdb_when_fonts_exist() {
        let xml = r#"<svg xmlns="http://www.w3.org/2000/svg" width="80" height="24">
  <text x="4" y="16" font-size="14">Hi</text>
</svg>"#;
        let img = match render(xml) {
            Ok(img) => img,
            Err(err) => panic!("text SVG should parse even without glyphs: {err:?}"),
        };
        assert_eq!(img.format, ImageFormat::Svg);
        if !system_fonts_loaded() {
            return;
        }
        let db = ensure_system_fonts();
        if db.is_empty() {
            return;
        }
        assert!(system_font_load_time().is_some());
        assert_eq!((img.source_width, img.source_height), (80, 24));
    }

    #[test]
    fn path_without_text_does_not_need_fontdb() {
        assert!(!svg_contains_text(blue_rect().as_bytes()));
        let source = prepare_source(blue_rect().as_bytes(), false, &limits()).unwrap();
        assert!(!source.wants_text);
        let _ = render(blue_rect()).unwrap();
    }

    #[test]
    fn svg_error_copy_matches_the_unsupported_view() {
        let err = SvgError::FileTooLarge {
            bytes: 9 * 1024 * 1024,
        };
        assert_eq!(
            err.user_message("icon.svg"),
            "icon.svg can't be previewed — this SVG is too large (9 MB)"
        );
        assert_eq!(err.header_status(), "Too large");
        assert_eq!(SvgError::Malformed.header_status(), "Damaged");
        assert_eq!(SvgError::TooDeep.header_status(), "Too complex");
        assert_eq!(SvgError::InvalidSize.header_status(), "Invalid size");
    }

    #[test]
    fn target_from_client_subtracts_the_header() {
        let target = svg_target_from_client(760, 520, 96);
        assert_eq!(target.width, 760);
        assert_eq!(target.height, 520 - 52);
        assert_eq!(target.dpi, 96);
        let scaled = svg_target_from_client(760, 520, 0);
        assert_eq!(scaled.dpi, 96);
    }

    fn sample_icon_svg() -> String {
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24">
  <path fill="steelblue" d="M4 4h16v16H4z"/>
  <circle cx="12" cy="12" r="5" fill="gold"/>
</svg>"#
            .to_string()
    }

    fn sample_illustration_svg() -> String {
        let mut xml = String::from(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="300" viewBox="0 0 400 300">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="1">
      <stop offset="0" stop-color="navy"/>
      <stop offset="1" stop-color="skyblue"/>
    </linearGradient>
  </defs>
"#,
        );
        for i in 0..300 {
            let x = (i % 20) * 20;
            let y = (i / 20) * 20;
            xml.push_str(&format!(
                r#"<rect x="{x}" y="{y}" width="18" height="18" fill="url(#g)" rx="2"/>"#
            ));
        }
        xml.push_str("</svg>");
        xml
    }

    fn sample_heavy_svg() -> String {
        let mut xml = String::from(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="800" height="600" viewBox="0 0 800 600">"#,
        );
        for i in 0..50_000 {
            let x = (i * 17) % 800;
            let y = (i * 13) % 600;
            xml.push_str(&format!(r#"<path d="M{x} {y}h2v2h-2z" fill="gray"/>"#));
        }
        xml.push_str("</svg>");
        xml
    }

    fn time_render(label: &str, xml: &str, target: SvgTarget) {
        let mut limits = limits();
        limits.target = target;
        let (token, gen) = live();
        let start = Instant::now();
        let img = render_svg(xml.as_bytes(), false, &limits, &token, gen).unwrap();
        let elapsed = start.elapsed();
        println!(
            "{label}: {:?} (Linux parse+render, {}x{} pixmap from {}x{} intrinsic, {} bytes source)",
            elapsed,
            img.width,
            img.height,
            img.source_width,
            img.source_height,
            xml.len()
        );
        assert!(!img.bgra.is_empty());
    }

    #[test]
    #[ignore]
    fn svg_render_timing_small_icon() {
        time_render(
            "svg small 24x24 path icon at 800x600 target",
            &sample_icon_svg(),
            SvgTarget {
                width: 800,
                height: 600,
                dpi: 96,
            },
        );
    }

    #[test]
    #[ignore]
    fn svg_render_timing_typical_illustration() {
        time_render(
            "svg typical illustration (~300 gradient rects) at 800x600 target",
            &sample_illustration_svg(),
            SvgTarget {
                width: 800,
                height: 600,
                dpi: 96,
            },
        );
    }

    #[test]
    #[ignore]
    fn svg_render_timing_heavy() {
        time_render(
            "svg heavy (50k paths) at 800x600 target",
            &sample_heavy_svg(),
            SvgTarget {
                width: 800,
                height: 600,
                dpi: 96,
            },
        );
    }

    #[test]
    #[ignore]
    fn fontdb_load_timing() {
        let start = Instant::now();
        let db = ensure_system_fonts();
        let elapsed = system_font_load_time().unwrap_or_else(|| start.elapsed());
        println!(
            "fontdb load_system_fonts: {:?} ({} faces, Linux; Windows will differ)",
            elapsed,
            db.len()
        );
    }
}
