//! Still-image preview that does not touch Win32.
//!
//! Magic bytes decide the format. The extension is ignored, so a `.txt` that
//! is a PNG decodes and a `.png` that is text does not. GIF and WebP contribute
//! the first frame only; nothing here loops an animation. An ICO with several
//! sizes contributes the largest frame that fits the limits. Limits are applied
//! before pixel buffers are allocated.

use std::cmp::Ordering;
use std::io::Cursor;

use crate::geometry::{effective_dpi, PxRect, BASE_DPI};
use crate::preview::{format_compact_size, format_grouped};

/// Image kinds this previewer decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Bmp,
    Gif,
    Ico,
    WebP,
}

const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
const JPEG_MAGIC: [u8; 3] = [0xFF, 0xD8, 0xFF];
/// `BITMAPFILEHEADER` is 14 bytes. The following u32 is the DIB header size.
const BMP_SNIFF_LEN: usize = 18;
const BMP_FILE_HEADER_LEN: u32 = 14;
/// DIB header sizes [`image`]'s BMP decoder accepts: CORE, INFO, V2, V3, V4, V5.
/// The 64-byte OS/2 `BITMAPCOREHEADER2` is not one of them.
const BMP_DIB_HEADER_SIZES: [u32; 6] = [12, 40, 52, 56, 108, 124];

/// `Some` when `header` starts with a supported image signature.
///
/// PNG, JPEG, GIF (`GIF87a` or `GIF89a`), WebP (`RIFF` at 0 and `WEBP` at 8),
/// ICO (`00 00 01 00` and a non-zero image count), and BMP. A BMP has to look
/// like a `BITMAPFILEHEADER`: the two reserved fields are zero and the DIB
/// header size is one the decoder accepts, so a file that merely starts with
/// the letters "BM" does not match. CUR (`00 00 02 00`) is not treated as ICO.
/// Short buffers and anything else are `None`. Bytes after the signature are
/// ignored, so a longer file still matches.
pub fn sniff_image(header: &[u8]) -> Option<ImageFormat> {
    if header.starts_with(&PNG_MAGIC) {
        Some(ImageFormat::Png)
    } else if header.starts_with(&JPEG_MAGIC) {
        Some(ImageFormat::Jpeg)
    } else if header.starts_with(b"GIF87a") || header.starts_with(b"GIF89a") {
        Some(ImageFormat::Gif)
    } else if sniff_webp(header) {
        Some(ImageFormat::WebP)
    } else if sniff_bmp(header) {
        Some(ImageFormat::Bmp)
    } else if sniff_ico(header) {
        Some(ImageFormat::Ico)
    } else {
        None
    }
}

fn sniff_webp(header: &[u8]) -> bool {
    header.len() >= 12 && header.starts_with(b"RIFF") && &header[8..12] == b"WEBP"
}

fn sniff_bmp(header: &[u8]) -> bool {
    if header.len() < BMP_SNIFF_LEN || !header.starts_with(b"BM") {
        return false;
    }
    // The spec requires the reserved bytes to be zero. The decoder ignores
    // them; requiring zero here is what drops ordinary text that starts "BM".
    if header[6..10] != [0, 0, 0, 0] {
        return false;
    }
    let off_bits = u32::from_le_bytes([header[10], header[11], header[12], header[13]]);
    let dib_size = u32::from_le_bytes([header[14], header[15], header[16], header[17]]);
    if !BMP_DIB_HEADER_SIZES.contains(&dib_size) {
        return false;
    }
    // Pixels start at or after the file header and the DIB header. A palette
    // only pushes the offset further out.
    off_bits >= BMP_FILE_HEADER_LEN.saturating_add(dib_size)
}

fn sniff_ico(header: &[u8]) -> bool {
    // ICONDIR: reserved 0, type 1, count > 0. Type 2 is a CUR file. Cursors
    // are not previewed; the hotspot is not a bitmap we show.
    header.len() >= 6
        && header[0] == 0
        && header[1] == 0
        && header[2] == 1
        && header[3] == 0
        && u16::from_le_bytes([header[4], header[5]]) > 0
}

/// Caps for one image preview. The defaults are the product limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    /// Refuse the file before reading it all when it is larger than this.
    pub max_file_bytes: u64,
    /// Refuse when `width * height` exceeds this, before decoding.
    pub max_pixels: u64,
    /// Refuse when either side exceeds this, before decoding.
    pub max_dimension: u32,
    /// Decoder allocation budget passed to [`image::Limits`].
    pub max_alloc_bytes: u64,
    /// Longest stored side. Larger bitmaps are downscaled to fit in this square.
    pub max_display_dimension: u32,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 64 * 1024 * 1024,
            max_pixels: 100_000_000,
            max_dimension: 30_000,
            max_alloc_bytes: 512 * 1024 * 1024,
            max_display_dimension: 4096,
        }
    }
}

/// Decoded bitmap. Pixels are top-down, 4 bytes each, BGRA, alpha composited
/// onto white so the buffer is opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    /// Stored width after a possible downscale.
    pub width: u32,
    /// Stored height after a possible downscale.
    pub height: u32,
    /// Width of the file before downscale.
    pub source_width: u32,
    /// Height of the file before downscale.
    pub source_height: u32,
    pub format: ImageFormat,
    pub bgra: Vec<u8>,
}

/// Why an image was not previewed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    /// Width, height, or pixel count is outside [`ImageLimits`].
    TooLarge { width: u32, height: u32 },
    /// The file is larger than [`ImageLimits::max_file_bytes`].
    FileTooLarge { bytes: u64 },
    /// The bytes are not a decodable image of the sniffed format.
    Corrupt,
    /// The decoder recognised the container but not this encoding.
    Unsupported,
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::TooLarge { width, height } => {
                write!(f, "image too large ({width}x{height})")
            }
            ImageError::FileTooLarge { bytes } => {
                write!(f, "image file too large ({bytes} bytes)")
            }
            ImageError::Corrupt => write!(f, "corrupt image"),
            ImageError::Unsupported => write!(f, "unsupported image"),
        }
    }
}

impl ImageError {
    /// Sentence for a person. [`Display`] stays the short technical form.
    pub fn user_message(&self) -> String {
        match self {
            ImageError::TooLarge { width, height } => format!(
                "This image is too large to preview ({} × {} pixels).",
                format_grouped(u64::from(*width)),
                format_grouped(u64::from(*height))
            ),
            ImageError::FileTooLarge { bytes } => format!(
                "This image is too large to preview ({}).",
                format_compact_size(*bytes)
            ),
            ImageError::Corrupt | ImageError::Unsupported => {
                "This image file looks damaged or isn't a supported image.".to_string()
            }
        }
    }

    /// Short second line for the header. The centred body uses [`Self::user_message`].
    pub fn header_status(&self) -> String {
        match self {
            ImageError::TooLarge { .. } | ImageError::FileTooLarge { .. } => {
                "Too large".to_string()
            }
            ImageError::Corrupt => "Damaged".to_string(),
            ImageError::Unsupported => "Not supported".to_string(),
        }
    }
}

/// Largest size that fits in `box_w` × `box_h` without changing the aspect
/// ratio or scaling up.
///
/// A zero source or a zero box is `(0, 0)`. Any other non-zero source is at
/// least 1×1. The maths stays in `u64` so the `u32` products cannot overflow.
pub fn fit_size(src_w: u32, src_h: u32, box_w: u32, box_h: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 || box_w == 0 || box_h == 0 {
        return (0, 0);
    }
    // Clamping the box to the source is the "never upscale" rule.
    let box_w = box_w.min(src_w);
    let box_h = box_h.min(src_h);
    let src_w64 = u64::from(src_w);
    let src_h64 = u64::from(src_h);
    let box_w64 = u64::from(box_w);
    let box_h64 = u64::from(box_h);
    // box_w/src_w <= box_h/src_h when the width product is the smaller one.
    if box_w64 * src_h64 <= box_h64 * src_w64 {
        let h = src_h64 * box_w64 / src_w64;
        (box_w, nonzero_side(h, box_h))
    } else {
        let w = src_w64 * box_h64 / src_h64;
        (nonzero_side(w, box_w), box_h)
    }
}

/// Integer division can round a very wide image down to 0. Keep a pixel.
fn nonzero_side(fitted: u64, cap: u32) -> u32 {
    u32::try_from(fitted).unwrap_or(cap).max(1)
}

/// Where the bitmap is drawn inside `body`.
///
/// The desired size is the source size scaled by `dpi / 96` (a missing DPI is
/// 96). [`fit_size`] then fits that box inside the body, so the picture is
/// never drawn larger than the DPI-scaled source. The result is centred. A
/// stored bitmap that was downscaled still uses the source size. An empty
/// body or a zero image is a zero-size rect at the body's origin.
pub fn image_dest_rect(
    body: PxRect,
    image_w: u32,
    image_h: u32,
    source_w: u32,
    source_h: u32,
    dpi: u32,
) -> PxRect {
    let origin = PxRect {
        left: body.left,
        top: body.top,
        right: body.left,
        bottom: body.top,
    };
    let body_w = body.width();
    let body_h = body.height();
    if body_w <= 0 || body_h <= 0 || image_w == 0 || image_h == 0 || source_w == 0 || source_h == 0
    {
        return origin;
    }
    let dpi = u64::from(effective_dpi(dpi));
    let base = u64::from(u32::try_from(BASE_DPI).unwrap_or(96));
    let desired_w = u64::from(source_w).saturating_mul(dpi) / base;
    let desired_h = u64::from(source_h).saturating_mul(dpi) / base;
    if desired_w == 0 || desired_h == 0 {
        return origin;
    }
    let (src_w, src_h) = shrink_to_u32(desired_w, desired_h);
    let (fit_w, fit_h) = fit_size(
        src_w,
        src_h,
        u32::try_from(body_w).unwrap_or(u32::MAX),
        u32::try_from(body_h).unwrap_or(u32::MAX),
    );
    if fit_w == 0 || fit_h == 0 {
        return origin;
    }
    let fit_w = i32::try_from(fit_w).unwrap_or(body_w);
    let fit_h = i32::try_from(fit_h).unwrap_or(body_h);
    let left = body.left.saturating_add(body_w.saturating_sub(fit_w) / 2);
    let top = body.top.saturating_add(body_h.saturating_sub(fit_h) / 2);
    PxRect {
        left,
        top,
        right: left.saturating_add(fit_w),
        bottom: top.saturating_add(fit_h),
    }
}

/// `fit_size` takes `u32`. A side past that is larger than any body, so keep
/// the aspect ratio and the larger side at `u32::MAX`.
fn shrink_to_u32(width: u64, height: u64) -> (u32, u32) {
    let cap = u64::from(u32::MAX);
    let max_side = width.max(height);
    if max_side <= cap {
        return (
            u32::try_from(width).unwrap_or(u32::MAX),
            u32::try_from(height).unwrap_or(u32::MAX),
        );
    }
    (
        u32::try_from(mul_div_u64(width, cap, max_side).max(1)).unwrap_or(u32::MAX),
        u32::try_from(mul_div_u64(height, cap, max_side).max(1)).unwrap_or(u32::MAX),
    )
}

/// `a * b / d` for a non-zero `d`. The product saturates instead of wrapping.
fn mul_div_u64(a: u64, b: u64, d: u64) -> u64 {
    if d == 0 {
        return u64::MAX;
    }
    let product = u128::from(a).saturating_mul(u128::from(b));
    u64::try_from(product / u128::from(d)).unwrap_or(u64::MAX)
}

/// Decode `bytes` as `format`, or reject it before pixel work when the header
/// claims a size outside `limits`.
///
/// GIF and WebP go through the still-image decoder, which yields one frame
/// (the first) and does not walk the rest of an animation. An ICO is reduced
/// to a single frame before that decoder runs; see [`ico_single_image`].
pub fn decode_image(
    bytes: &[u8],
    format: ImageFormat,
    limits: &ImageLimits,
) -> Result<DecodedImage, ImageError> {
    let chosen_ico;
    let bytes = if format == ImageFormat::Ico {
        chosen_ico = ico_single_image(bytes, limits)?;
        chosen_ico.as_slice()
    } else {
        bytes
    };

    let (width, height) = probe_dimensions(bytes, format, limits)?;
    if width == 0 || height == 0 {
        return Err(ImageError::Corrupt);
    }
    if exceeds_geometry(width, height, limits) {
        return Err(ImageError::TooLarge { width, height });
    }

    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image_format(format));
    reader.limits(decoder_limits(limits));
    let decoded = reader
        .decode()
        .map_err(|err| classify_decode_error(err, width, height))?;

    let rgba = decoded.into_rgba8();
    let source_width = rgba.width();
    let source_height = rgba.height();
    if source_width == 0 || source_height == 0 {
        return Err(ImageError::Corrupt);
    }
    if exceeds_geometry(source_width, source_height, limits) {
        return Err(ImageError::TooLarge {
            width: source_width,
            height: source_height,
        });
    }

    let (out_w, out_h) = fit_size(
        source_width,
        source_height,
        limits.max_display_dimension,
        limits.max_display_dimension,
    );
    if out_w == 0 || out_h == 0 {
        return Err(ImageError::TooLarge {
            width: source_width,
            height: source_height,
        });
    }
    let rgba = if out_w == source_width && out_h == source_height {
        rgba
    } else {
        image::imageops::resize(&rgba, out_w, out_h, image::imageops::FilterType::Triangle)
    };
    let bgra = composite_bgra_on_white(rgba.as_raw())?;
    let stored = u64::from(rgba.width()) * u64::from(rgba.height()) * 4;
    if bgra.len() as u64 != stored {
        return Err(ImageError::Corrupt);
    }
    Ok(DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        source_width,
        source_height,
        format,
        bgra,
    })
}

fn exceeds_geometry(width: u32, height: u32, limits: &ImageLimits) -> bool {
    width > limits.max_dimension
        || height > limits.max_dimension
        || u64::from(width) * u64::from(height) > limits.max_pixels
}

fn image_format(format: ImageFormat) -> image::ImageFormat {
    match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
        ImageFormat::Gif => image::ImageFormat::Gif,
        ImageFormat::Ico => image::ImageFormat::Ico,
        ImageFormat::WebP => image::ImageFormat::WebP,
    }
}

fn decoder_limits(limits: &ImageLimits) -> image::Limits {
    let mut decoder_limits = image::Limits::default();
    decoder_limits.max_image_width = Some(limits.max_dimension);
    decoder_limits.max_image_height = Some(limits.max_dimension);
    decoder_limits.max_alloc = Some(limits.max_alloc_bytes);
    decoder_limits
}

/// Read the size without expanding pixels.
///
/// A container header that is already outside `limits` is returned immediately,
/// so the decoder is not asked to size a buffer it will not be allowed to fill.
/// Otherwise [`image::ImageReader::into_dimensions`] is the normal path. A PNG
/// whose `IHDR` is the whole file never gets a size from the decoder: the PNG
/// decoder only reports one after it has seen the start of `IDAT`. The same
/// fallback covers a BMP, GIF, or WebP header the decoder rejects. The chunk
/// is then read here so an oversized claim is still [`ImageError::TooLarge`]
/// and is not decoded.
fn probe_dimensions(
    bytes: &[u8],
    format: ImageFormat,
    limits: &ImageLimits,
) -> Result<(u32, u32), ImageError> {
    if let Some((width, height)) = container_dimensions(bytes, format) {
        if width == 0 || height == 0 || exceeds_geometry(width, height, limits) {
            return Ok((width, height));
        }
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image_format(format));
    reader.no_limits();
    match reader.into_dimensions() {
        Ok(dims) => Ok(dims),
        Err(err) => match container_dimensions(bytes, format) {
            Some(dims) => Ok(dims),
            None => Err(classify_decode_error(err, 0, 0)),
        },
    }
}

fn container_dimensions(bytes: &[u8], format: ImageFormat) -> Option<(u32, u32)> {
    match format {
        ImageFormat::Png => png_ihdr_dimensions(bytes),
        // JPEG sizes come from ImageReader. A truncated file has no reliable
        // size once that header parse has already failed.
        ImageFormat::Jpeg => None,
        ImageFormat::Bmp => bmp_file_dimensions(bytes),
        ImageFormat::Gif => gif_screen_dimensions(bytes),
        ImageFormat::WebP => webp_canvas_dimensions(bytes),
        // ICO sizes are chosen in [`ico_single_image`] before this runs. The
        // directory byte is not the PNG's real size, so it is not a fallback.
        ImageFormat::Ico => None,
    }
}

fn bmp_file_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < BMP_SNIFF_LEN || !bytes.starts_with(b"BM") {
        return None;
    }
    let dib_size = u32::from_le_bytes(bytes.get(14..18)?.try_into().ok()?);
    if dib_size == 12 {
        // BITMAPCOREHEADER stores width and height as u16.
        let width = u16::from_le_bytes(bytes.get(18..20)?.try_into().ok()?);
        let height = u16::from_le_bytes(bytes.get(20..22)?.try_into().ok()?);
        return Some((u32::from(width), u32::from(height)));
    }
    if !BMP_DIB_HEADER_SIZES.contains(&dib_size) {
        return None;
    }
    let width = i32::from_le_bytes(bytes.get(18..22)?.try_into().ok()?);
    let height = i32::from_le_bytes(bytes.get(22..26)?.try_into().ok()?);
    if width < 0 {
        return None;
    }
    // A negative height is a top-down DIB. `i32::MIN` has no positive twin.
    let height = if height == i32::MIN {
        return None;
    } else {
        height.unsigned_abs()
    };
    Some((u32::try_from(width).ok()?, height))
}

fn gif_screen_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let rest = bytes
        .strip_prefix(b"GIF87a")
        .or_else(|| bytes.strip_prefix(b"GIF89a"))?;
    let width = u16::from_le_bytes(rest.get(..2)?.try_into().ok()?);
    let height = u16::from_le_bytes(rest.get(2..4)?.try_into().ok()?);
    Some((u32::from(width), u32::from(height)))
}

/// Canvas size from a `VP8X` or `VP8L` chunk. Lossy `VP8 ` is left to the decoder.
fn webp_canvas_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if !sniff_webp(bytes) {
        return None;
    }
    let mut offset = 12;
    while let Some(header) = bytes.get(offset..offset + 8) {
        let tag = &header[..4];
        let size = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
        let data = offset + 8;
        if tag == b"VP8X" {
            let canvas = bytes.get(data..data + 10)?;
            let width =
                u32::from(canvas[4]) | (u32::from(canvas[5]) << 8) | (u32::from(canvas[6]) << 16);
            let height =
                u32::from(canvas[7]) | (u32::from(canvas[8]) << 8) | (u32::from(canvas[9]) << 16);
            return Some((width.saturating_add(1), height.saturating_add(1)));
        }
        if tag == b"VP8L" {
            let payload = bytes.get(data..data + 5)?;
            if payload[0] != 0x2F {
                return None;
            }
            // 14-bit width-1, then 14-bit height-1, little-endian bit pack.
            let bits = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
            let width = (bits & 0x3FFF) + 1;
            let height = ((bits >> 14) & 0x3FFF) + 1;
            return Some((width, height));
        }
        let padded = size.checked_add(size & 1)?;
        offset = data.checked_add(padded)?;
    }
    None
}

const ICO_DIR_LEN: usize = 6;
const ICO_ENTRY_LEN: usize = 16;

struct IcoEntry {
    width: u32,
    height: u32,
    bits_per_pixel: u16,
    /// Directory order. Equal frames keep the earlier one.
    index: usize,
    raw: [u8; ICO_ENTRY_LEN],
    image_offset: u32,
    image_length: u32,
}

/// One ICO image, chosen and repacked so the decoder cannot pick a different one.
///
/// Several sizes are common. The frame kept is the one with the most pixels
/// that still fits [`ImageLimits`] (each side, and the pixel count). An equal
/// area keeps the higher bit depth, then the earlier directory entry. A PNG
/// payload's real size comes from its `IHDR`: the directory stores 0 for a
/// side of 256, and a PNG may be larger than that. A BMP payload uses the
/// directory size, which is at most 256. When nothing fits, the error names
/// the smallest frame. CUR (`type == 2`) is rejected.
///
/// The `image` crate's own picker prefers bit depth over area and does not
/// know these limits, so the chosen entry is rewritten as a one-image ICO
/// before that decoder runs. Only that frame's compressed bytes are copied.
/// The pixel buffer is allocated later, after the usual limit check.
fn ico_single_image(bytes: &[u8], limits: &ImageLimits) -> Result<Vec<u8>, ImageError> {
    let entries = ico_entries(bytes)?;
    let chosen = choose_ico_entry(&entries, limits)?;
    let payload = ico_payload(bytes, chosen).ok_or(ImageError::Corrupt)?;
    let mut raw = chosen.raw;
    let length = u32::try_from(payload.len()).map_err(|_| ImageError::Corrupt)?;
    raw[8..12].copy_from_slice(&length.to_le_bytes());
    // ICONDIR (6) + one ICONDIRENTRY (16).
    raw[12..16].copy_from_slice(&22u32.to_le_bytes());

    let mut out = Vec::with_capacity(ICO_DIR_LEN + ICO_ENTRY_LEN + payload.len());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&raw);
    out.extend_from_slice(payload);
    Ok(out)
}

fn ico_entries(bytes: &[u8]) -> Result<Vec<IcoEntry>, ImageError> {
    if bytes.len() < ICO_DIR_LEN || !sniff_ico(bytes) {
        return Err(ImageError::Corrupt);
    }
    let count = usize::from(u16::from_le_bytes([bytes[4], bytes[5]]));
    let dir_bytes = count
        .checked_mul(ICO_ENTRY_LEN)
        .ok_or(ImageError::Corrupt)?;
    let dir_end = ICO_DIR_LEN
        .checked_add(dir_bytes)
        .ok_or(ImageError::Corrupt)?;
    if bytes.len() < dir_end {
        return Err(ImageError::Corrupt);
    }
    let mut entries = Vec::new();
    for index in 0..count {
        let start = ICO_DIR_LEN + index * ICO_ENTRY_LEN;
        let mut raw = [0u8; ICO_ENTRY_LEN];
        raw.copy_from_slice(&bytes[start..start + ICO_ENTRY_LEN]);
        let bits_per_pixel = u16::from_le_bytes([raw[6], raw[7]]);
        let image_length = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
        let image_offset = u32::from_le_bytes([raw[12], raw[13], raw[14], raw[15]]);
        let Some((width, height)) = ico_frame_size(bytes, &raw, image_offset, image_length) else {
            continue;
        };
        entries.push(IcoEntry {
            width,
            height,
            bits_per_pixel,
            index,
            raw,
            image_offset,
            image_length,
        });
    }
    Ok(entries)
}

fn ico_payload<'a>(bytes: &'a [u8], entry: &IcoEntry) -> Option<&'a [u8]> {
    let start = usize::try_from(entry.image_offset).ok()?;
    let len = usize::try_from(entry.image_length).ok()?;
    let end = start.checked_add(len)?;
    bytes.get(start..end)
}

/// PNG entries use `IHDR`. Anything else uses the directory, where 0 means 256.
fn ico_frame_size(bytes: &[u8], raw: &[u8], offset: u32, length: u32) -> Option<(u32, u32)> {
    let start = usize::try_from(offset).ok()?;
    let len = usize::try_from(length).ok()?;
    if len == 0 {
        return None;
    }
    let end = start.checked_add(len)?;
    let payload = bytes.get(start..end)?;
    if let Some((width, height)) = png_ihdr_dimensions(payload) {
        if width == 0 || height == 0 {
            return None;
        }
        return Some((width, height));
    }
    let width = match raw[0] {
        0 => 256,
        n => u32::from(n),
    };
    let height = match raw[1] {
        0 => 256,
        n => u32::from(n),
    };
    Some((width, height))
}

fn choose_ico_entry<'a>(
    entries: &'a [IcoEntry],
    limits: &ImageLimits,
) -> Result<&'a IcoEntry, ImageError> {
    let mut best_fit: Option<&IcoEntry> = None;
    let mut smallest: Option<&IcoEntry> = None;
    for entry in entries {
        smallest = Some(match smallest {
            Some(prev) if ico_is_smaller(entry, prev) => entry,
            Some(prev) => prev,
            None => entry,
        });
        if exceeds_geometry(entry.width, entry.height, limits) {
            continue;
        }
        best_fit = Some(match best_fit {
            Some(prev) if ico_is_preferred(entry, prev) => entry,
            Some(prev) => prev,
            None => entry,
        });
    }
    if let Some(best) = best_fit {
        return Ok(best);
    }
    match smallest {
        Some(entry) => Err(ImageError::TooLarge {
            width: entry.width,
            height: entry.height,
        }),
        None => Err(ImageError::Corrupt),
    }
}

/// Larger area wins. Ties prefer a higher bit depth, then the earlier entry.
fn ico_is_preferred(candidate: &IcoEntry, current: &IcoEntry) -> bool {
    match ico_area(candidate).cmp(&ico_area(current)) {
        Ordering::Greater => true,
        Ordering::Less => false,
        Ordering::Equal => match candidate.bits_per_pixel.cmp(&current.bits_per_pixel) {
            Ordering::Greater => true,
            Ordering::Less => false,
            Ordering::Equal => candidate.index < current.index,
        },
    }
}

fn ico_is_smaller(candidate: &IcoEntry, current: &IcoEntry) -> bool {
    match ico_area(candidate).cmp(&ico_area(current)) {
        Ordering::Less => true,
        Ordering::Greater => false,
        Ordering::Equal => candidate.index < current.index,
    }
}

fn ico_area(entry: &IcoEntry) -> u64 {
    u64::from(entry.width) * u64::from(entry.height)
}

fn png_ihdr_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let rest = bytes.strip_prefix(&PNG_MAGIC)?;
    let len = be_u32(rest.get(..4)?)?;
    if len != 13 {
        return None;
    }
    let chunk = rest.get(4..)?;
    if chunk.get(..4)? != b"IHDR" {
        return None;
    }
    let width = be_u32(chunk.get(4..8)?)?;
    let height = be_u32(chunk.get(8..12)?)?;
    Some((width, height))
}

fn be_u32(bytes: &[u8]) -> Option<u32> {
    let arr: [u8; 4] = bytes.try_into().ok()?;
    Some(u32::from_be_bytes(arr))
}

fn classify_decode_error(err: image::ImageError, width: u32, height: u32) -> ImageError {
    match err {
        image::ImageError::Limits(_) => ImageError::TooLarge { width, height },
        image::ImageError::Unsupported(_) => ImageError::Unsupported,
        _ => ImageError::Corrupt,
    }
}

/// Straight RGBA onto white, written B, G, R, 255.
fn composite_bgra_on_white(rgba: &[u8]) -> Result<Vec<u8>, ImageError> {
    if rgba.len() % 4 != 0 {
        return Err(ImageError::Corrupt);
    }
    let mut bgra = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        let alpha = px[3];
        bgra.push(over_white(px[2], alpha));
        bgra.push(over_white(px[1], alpha));
        bgra.push(over_white(px[0], alpha));
        bgra.push(255);
    }
    Ok(bgra)
}

/// `src` over white. Half-up rounding, and an opaque pixel stays itself.
fn over_white(channel: u8, alpha: u8) -> u8 {
    let channel = u32::from(channel);
    let alpha = u32::from(alpha);
    let inv = 255 - alpha;
    let mixed = (channel * alpha + 255 * inv + 127) / 255;
    u8::try_from(mixed).unwrap_or(255)
}

/// Header details such as `1.2 MB · 4,000 × 3,000 · JPEG`.
///
/// The dimensions are the source size, before any downscale. The size uses
/// the same compact binary units as the text header.
pub fn format_image_details(total_bytes: u64, img: &DecodedImage) -> String {
    let kind = match img.format {
        ImageFormat::Png => "PNG",
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Bmp => "BMP",
        ImageFormat::Gif => "GIF",
        ImageFormat::Ico => "ICO",
        ImageFormat::WebP => "WebP",
    };
    format!(
        "{} · {} × {} · {kind}",
        format_compact_size(total_bytes),
        format_grouped(u64::from(img.source_width)),
        format_grouped(u64::from(img.source_height))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::bmp::BmpEncoder;
    use image::codecs::gif::GifEncoder;
    use image::codecs::ico::{IcoEncoder, IcoFrame};
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::codecs::webp::WebPEncoder;
    use image::{Delay, ExtendedColorType, Frame, ImageEncoder, RgbaImage};

    fn limits() -> ImageLimits {
        ImageLimits::default()
    }

    fn encode(
        width: u32,
        height: u32,
        pixels: &[u8],
        color: ExtendedColorType,
        jpeg: bool,
    ) -> Vec<u8> {
        let mut buf = Vec::new();
        if jpeg {
            JpegEncoder::new_with_quality(&mut buf, 100)
                .write_image(pixels, width, height, color)
                .unwrap();
        } else {
            PngEncoder::new(&mut buf)
                .write_image(pixels, width, height, color)
                .unwrap();
        }
        buf
    }

    fn rgba_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        encode(width, height, rgba, ExtendedColorType::Rgba8, false)
    }

    fn rgb_png(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
        encode(width, height, rgb, ExtendedColorType::Rgb8, false)
    }

    fn rgb_jpeg(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
        encode(width, height, rgb, ExtendedColorType::Rgb8, true)
    }

    /// IHDR only. No `IDAT`, so a decoder that inflates pixels cannot succeed.
    fn crafted_png(width: u32, height: u32) -> Vec<u8> {
        let mut ihdr = Vec::with_capacity(17);
        ihdr.extend_from_slice(b"IHDR");
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let crc = png_crc(&ihdr);
        let mut data = Vec::with_capacity(8 + 4 + ihdr.len() + 4);
        data.extend_from_slice(&PNG_MAGIC);
        data.extend_from_slice(&13u32.to_be_bytes());
        data.extend_from_slice(&ihdr);
        data.extend_from_slice(&crc.to_be_bytes());
        data
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

    #[test]
    fn sniff_matches_magic_bytes_only() {
        assert_eq!(sniff_image(&PNG_MAGIC), Some(ImageFormat::Png));
        let mut longer = PNG_MAGIC.to_vec();
        longer.extend_from_slice(&[0, 1, 2, 3]);
        assert_eq!(sniff_image(&longer), Some(ImageFormat::Png));
        assert_eq!(sniff_image(&JPEG_MAGIC), Some(ImageFormat::Jpeg));
        assert_eq!(
            sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00]),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(sniff_image(b"hello"), None);
        assert_eq!(sniff_image(b"not a png"), None);
        assert_eq!(sniff_image(b""), None);
        assert_eq!(
            sniff_image(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A]),
            None
        );
        assert_eq!(sniff_image(&[0xFF, 0xD8]), None);
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0x00]), None);
        assert_eq!(sniff_image(&[0x00]), None);
        // The signature has to be the prefix. A PNG buried in text is text.
        let mut buried = b"note ".to_vec();
        buried.extend_from_slice(&PNG_MAGIC);
        assert_eq!(sniff_image(&buried), None);
    }

    #[test]
    fn default_limits_are_the_product_budget() {
        let limits = ImageLimits::default();
        assert_eq!(limits.max_file_bytes, 64 * 1024 * 1024);
        assert_eq!(limits.max_pixels, 100_000_000);
        assert_eq!(limits.max_dimension, 30_000);
        assert_eq!(limits.max_alloc_bytes, 512 * 1024 * 1024);
        assert_eq!(limits.max_display_dimension, 4096);
    }

    #[test]
    fn fit_size_keeps_aspect_and_never_upscales() {
        assert_eq!(fit_size(4_000, 2_000, 1_000, 1_000), (1_000, 500));
        assert_eq!(fit_size(2_000, 4_000, 1_000, 1_000), (500, 1_000));
        assert_eq!(fit_size(800, 800, 400, 400), (400, 400));
        assert_eq!(fit_size(10, 20, 1_000, 1_000), (10, 20));
        assert_eq!(fit_size(1, 1, 5, 5), (1, 1));
        assert_eq!(fit_size(2, 2, 1, 1), (1, 1));
        assert_eq!(fit_size(100, 50, 80, 1_000), (80, 40));
        assert_eq!(fit_size(100, 50, 1_000, 30), (60, 30));
        assert_eq!(fit_size(1_920, 1_080, 800, 600), (800, 450));
    }

    #[test]
    fn fit_size_one_pixel_extremes_and_zeros() {
        assert_eq!(fit_size(100_000, 1, 50, 50), (50, 1));
        assert_eq!(fit_size(1, 100_000, 50, 50), (1, 50));
        assert_eq!(fit_size(10_000, 1, 1, 1), (1, 1));
        assert_eq!(fit_size(1, 10_000, 1, 1), (1, 1));
        assert_eq!(fit_size(u32::MAX, u32::MAX, u32::MAX, 1), (1, 1));
        assert_eq!(fit_size(u32::MAX, u32::MAX, 1, u32::MAX), (1, 1));
        assert_eq!(fit_size(u32::MAX, 2, u32::MAX, 1), (u32::MAX / 2, 1));
        assert_eq!(fit_size(u32::MAX, 1, u32::MAX, u32::MAX), (u32::MAX, 1));
        assert_eq!(fit_size(0, 10, 10, 10), (0, 0));
        assert_eq!(fit_size(10, 0, 10, 10), (0, 0));
        assert_eq!(fit_size(10, 10, 0, 10), (0, 0));
        assert_eq!(fit_size(10, 10, 10, 0), (0, 0));
        assert_eq!(fit_size(0, 0, 0, 0), (0, 0));
    }

    #[test]
    fn rgb_png_is_exact_bgra() {
        let png = rgb_png(2, 2, &[10, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30]);
        let decoded = decode_image(&png, ImageFormat::Png, &limits()).unwrap();
        assert_eq!(decoded.format, ImageFormat::Png);
        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!((decoded.source_width, decoded.source_height), (2, 2));
        assert_eq!(decoded.bgra, [30, 20, 10, 255].repeat(4));
    }

    #[test]
    fn rgba_png_composites_onto_white_in_bgra_order() {
        // Opaque blue, fully transparent blue, and blue at alpha 128.
        let png = rgba_png(
            3,
            1,
            &[
                0, 0, 255, 255, // blue
                0, 0, 255, 0, // transparent → white
                0, 0, 255, 128, // half blue over white
            ],
        );
        let decoded = decode_image(&png, ImageFormat::Png, &limits()).unwrap();
        assert_eq!(
            decoded.bgra,
            vec![
                255, 0, 0, 255, // B, G, R, A
                255, 255, 255, 255, 255, 127, 127, 255,
            ]
        );
    }

    #[test]
    fn small_jpeg_keeps_dimensions_and_approximate_colour() {
        let mut rgb = Vec::new();
        for _ in 0..(16 * 16) {
            rgb.extend_from_slice(&[220, 10, 20]);
        }
        let jpeg = rgb_jpeg(16, 16, &rgb);
        assert_eq!(sniff_image(&jpeg), Some(ImageFormat::Jpeg));
        let decoded = decode_image(&jpeg, ImageFormat::Jpeg, &limits()).unwrap();
        assert_eq!(decoded.format, ImageFormat::Jpeg);
        assert_eq!((decoded.width, decoded.height), (16, 16));
        assert_eq!((decoded.source_width, decoded.source_height), (16, 16));
        assert_eq!(decoded.bgra.len(), 16 * 16 * 4);
        let px = &decoded.bgra[..4];
        assert!(px[2] > 180, "red {}", px[2]);
        assert!(px[1] < 50, "green {}", px[1]);
        assert!(px[0] < 60, "blue {}", px[0]);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn downscale_fits_the_display_box_and_keeps_aspect() {
        let mut rgb = vec![0u8; 32 * 16 * 3];
        for px in rgb.chunks_exact_mut(3) {
            px.copy_from_slice(&[0, 180, 40]);
        }
        let png = rgb_png(32, 16, &rgb);
        let limits = ImageLimits {
            max_display_dimension: 8,
            ..ImageLimits::default()
        };
        let decoded = decode_image(&png, ImageFormat::Png, &limits).unwrap();
        assert_eq!(fit_size(32, 16, 8, 8), (8, 4));
        assert_eq!((decoded.source_width, decoded.source_height), (32, 16));
        assert_eq!((decoded.width, decoded.height), (8, 4));
        assert_eq!(decoded.bgra.len(), 8 * 4 * 4);
        assert!(decoded.bgra.chunks_exact(4).all(|px| px[3] == 255));
        // Green from the source survives the triangle filter.
        assert!(decoded.bgra[1] > 100, "green {}", decoded.bgra[1]);
    }

    #[test]
    fn small_image_is_not_upscaled_to_the_display_box() {
        let png = rgb_png(2, 2, &[1, 2, 3, 1, 2, 3, 1, 2, 3, 1, 2, 3]);
        let decoded = decode_image(&png, ImageFormat::Png, &limits()).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 2));
    }

    #[test]
    fn crafted_header_is_rejected_for_pixels_and_sides_without_decoding() {
        // 20_000 × 20_000 is under the 30_000 side cap and over 100_000_000 pixels.
        // There is no IDAT, so a decode would be Corrupt rather than TooLarge.
        let huge = crafted_png(20_000, 20_000);
        assert_eq!(
            decode_image(&huge, ImageFormat::Png, &limits()).unwrap_err(),
            ImageError::TooLarge {
                width: 20_000,
                height: 20_000
            }
        );
        // 40_000 × 2 is few pixels and one side over the cap.
        let wide = crafted_png(40_000, 2);
        assert_eq!(
            decode_image(&wide, ImageFormat::Png, &limits()).unwrap_err(),
            ImageError::TooLarge {
                width: 40_000,
                height: 2
            }
        );
        let absurd = crafted_png(u32::MAX, u32::MAX);
        assert_eq!(
            decode_image(&absurd, ImageFormat::Png, &limits()).unwrap_err(),
            ImageError::TooLarge {
                width: u32::MAX,
                height: u32::MAX
            }
        );
    }

    #[test]
    fn geometry_limits_apply_to_a_real_image_before_it_is_stored() {
        let png = rgb_png(2, 2, &[9, 8, 7, 9, 8, 7, 9, 8, 7, 9, 8, 7]);
        let too_many = ImageLimits {
            max_pixels: 3,
            ..ImageLimits::default()
        };
        assert_eq!(
            decode_image(&png, ImageFormat::Png, &too_many).unwrap_err(),
            ImageError::TooLarge {
                width: 2,
                height: 2
            }
        );
        let too_wide = ImageLimits {
            max_dimension: 1,
            ..ImageLimits::default()
        };
        assert_eq!(
            decode_image(&png, ImageFormat::Png, &too_wide).unwrap_err(),
            ImageError::TooLarge {
                width: 2,
                height: 2
            }
        );
        let exact = ImageLimits {
            max_pixels: 4,
            max_dimension: 2,
            ..ImageLimits::default()
        };
        let decoded = decode_image(&png, ImageFormat::Png, &exact).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 2));
    }

    #[test]
    fn header_only_png_inside_the_limits_is_corrupt() {
        let err = decode_image(&crafted_png(1, 1), ImageFormat::Png, &limits()).unwrap_err();
        assert_eq!(err, ImageError::Corrupt);
        assert_eq!(
            decode_image(&crafted_png(0, 4), ImageFormat::Png, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(&crafted_png(4, 0), ImageFormat::Png, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
    }

    #[test]
    fn alloc_budget_is_a_too_large_error() {
        let png = rgb_png(16, 16, &vec![40u8; 16 * 16 * 3]);
        let tight = ImageLimits {
            max_alloc_bytes: 32,
            ..ImageLimits::default()
        };
        assert_eq!(
            decode_image(&png, ImageFormat::Png, &tight).unwrap_err(),
            ImageError::TooLarge {
                width: 16,
                height: 16
            }
        );
    }

    #[test]
    fn truncated_and_foreign_bytes_are_corrupt() {
        let png = rgb_png(2, 2, &[1, 2, 3, 1, 2, 3, 1, 2, 3, 1, 2, 3]);
        assert!(png.len() > 8);
        assert_eq!(
            decode_image(&png[..png.len() / 2], ImageFormat::Png, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(b"", ImageFormat::Png, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(b"this is not an image", ImageFormat::Png, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(b"this is not an image", ImageFormat::Jpeg, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(&[0xFF, 0xD8, 0xFF], ImageFormat::Jpeg, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
    }

    #[test]
    fn error_messages_and_header_details() {
        let too_big = ImageError::TooLarge {
            width: 12_000,
            height: 9_000,
        };
        assert_eq!(
            too_big.user_message(),
            "This image is too large to preview (12,000 × 9,000 pixels)."
        );
        assert_eq!(too_big.header_status(), "Too large");
        assert_eq!(too_big.to_string(), "image too large (12000x9000)");

        let file = ImageError::FileTooLarge { bytes: 1_258_291 };
        assert_eq!(
            file.user_message(),
            "This image is too large to preview (1.2 MB)."
        );
        assert_eq!(file.header_status(), "Too large");

        let damaged = "This image file looks damaged or isn't a supported image.";
        assert_eq!(ImageError::Corrupt.user_message(), damaged);
        assert_eq!(ImageError::Unsupported.user_message(), damaged);
        assert_eq!(ImageError::Corrupt.header_status(), "Damaged");
        assert_eq!(ImageError::Unsupported.header_status(), "Not supported");
        assert_ne!(
            ImageError::Corrupt.user_message(),
            ImageError::Corrupt.to_string()
        );

        let jpeg = DecodedImage {
            width: 800,
            height: 600,
            source_width: 4_000,
            source_height: 3_000,
            format: ImageFormat::Jpeg,
            bgra: vec![],
        };
        assert_eq!(
            format_image_details(1_258_291, &jpeg),
            "1.2 MB · 4,000 × 3,000 · JPEG"
        );
        let png = DecodedImage {
            width: 8,
            height: 8,
            source_width: 8,
            source_height: 8,
            format: ImageFormat::Png,
            bgra: vec![],
        };
        assert_eq!(format_image_details(512, &png), "512 B · 8 × 8 · PNG");
        assert_eq!(format_image_details(0, &png), "0 B · 8 × 8 · PNG");

        for (format, label) in [
            (ImageFormat::Bmp, "BMP"),
            (ImageFormat::Gif, "GIF"),
            (ImageFormat::Ico, "ICO"),
            (ImageFormat::WebP, "WebP"),
        ] {
            let image = DecodedImage {
                format,
                ..png.clone()
            };
            assert_eq!(
                format_image_details(10, &image),
                format!("10 B · 8 × 8 · {label}")
            );
        }
    }

    fn body(left: i32, top: i32, right: i32, bottom: i32) -> PxRect {
        PxRect {
            left,
            top,
            right,
            bottom,
        }
    }

    #[test]
    fn image_dest_rect_centres_and_letterboxes() {
        // 200×100 body. A matching landscape source fills it.
        let wide = body(10, 20, 210, 120);
        assert_eq!(image_dest_rect(wide, 200, 100, 200, 100, 96), wide);

        // Portrait source: bars on the left and right. (200-50)/2 = 75.
        assert_eq!(
            image_dest_rect(wide, 100, 200, 100, 200, 96),
            body(85, 20, 135, 120)
        );
        // Landscape source shorter than the body: bars above and below.
        assert_eq!(
            image_dest_rect(wide, 200, 50, 200, 50, 96),
            body(10, 45, 210, 95)
        );

        // Odd leftover pixel stays on the right and bottom (integer centre).
        let odd = body(0, 0, 101, 51);
        assert_eq!(
            image_dest_rect(odd, 10, 10, 10, 10, 96),
            body(45, 20, 55, 30)
        );

        // Negative origin, still centred inside the body.
        let shifted = body(-20, -10, 180, 90);
        assert_eq!(
            image_dest_rect(shifted, 100, 50, 100, 50, 96),
            body(30, 15, 130, 65)
        );
    }

    #[test]
    fn image_dest_rect_scales_a_small_image_with_dpi() {
        let area = body(0, 0, 500, 500);
        // Truncating division: 10 * dpi / 96, never rounded up, never stretched
        // to the body.
        assert_eq!(
            image_dest_rect(area, 10, 8, 10, 8, 96),
            body(245, 246, 255, 254)
        );
        assert_eq!(
            image_dest_rect(area, 10, 8, 10, 8, 144),
            body(242, 244, 257, 256)
        );
        assert_eq!(
            image_dest_rect(area, 10, 8, 10, 8, 192),
            body(240, 242, 260, 258)
        );
        // A missing DPI is 96. 1×1 at 144 DPI is 1×1 (1.5 truncates), at 192 it is 2×2.
        assert_eq!(
            image_dest_rect(area, 10, 8, 10, 8, 0),
            image_dest_rect(area, 10, 8, 10, 8, 96)
        );
        assert_eq!(
            image_dest_rect(area, 1, 1, 1, 1, 144),
            body(249, 249, 250, 250)
        );
        assert_eq!(
            image_dest_rect(area, 1, 1, 1, 1, 192),
            body(249, 249, 251, 251)
        );

        // The DPI-scaled source is larger than the body, so it letterboxes.
        let tight = body(0, 0, 100, 80);
        // 200×100 at 192 DPI wants 400×200, which fits as 100×50.
        assert_eq!(
            image_dest_rect(tight, 200, 100, 200, 100, 192),
            body(0, 15, 100, 65)
        );
    }

    #[test]
    fn downscaled_buffer_still_fills_from_the_source_size() {
        let area = body(0, 0, 1_000, 1_000);
        let stored = image_dest_rect(area, 8, 4, 400, 200, 96);
        let full = image_dest_rect(area, 400, 200, 400, 200, 96);
        assert_eq!(stored, full);
        assert_eq!(stored, body(300, 400, 700, 600));

        // 192 DPI wants 800×400 from the source, not 16×8 from the buffer.
        assert_eq!(
            image_dest_rect(area, 8, 4, 400, 200, 192),
            image_dest_rect(area, 400, 200, 400, 200, 192)
        );
        assert_eq!(
            image_dest_rect(area, 8, 4, 400, 200, 192),
            body(100, 300, 900, 700)
        );
    }

    #[test]
    fn empty_or_negative_body_and_zero_image_are_a_point() {
        let empty = body(5, 7, 5, 100);
        assert_eq!(image_dest_rect(empty, 10, 10, 10, 10, 96), body(5, 7, 5, 7));
        let negative = body(40, 50, 10, 20);
        assert_eq!(
            image_dest_rect(negative, 10, 10, 10, 10, 192),
            body(40, 50, 40, 50)
        );
        let area = body(3, 4, 100, 80);
        let origin = body(3, 4, 3, 4);
        assert_eq!(image_dest_rect(area, 0, 10, 10, 10, 96), origin);
        assert_eq!(image_dest_rect(area, 10, 0, 10, 10, 96), origin);
        assert_eq!(image_dest_rect(area, 10, 10, 0, 10, 96), origin);
        assert_eq!(image_dest_rect(area, 10, 10, 10, 0, 96), origin);
        // 1×1 at 1 DPI scales to 0 and stays a point. It is not bumped up to 1.
        assert_eq!(image_dest_rect(area, 1, 1, 1, 1, 1), origin);
    }

    #[test]
    fn image_dest_rect_huge_values_do_not_overflow() {
        let area = body(0, 0, 100, 40);
        // Both sides saturate to the same u64 before the divide, so the
        // picture is square and the short side of the body wins.
        assert_eq!(
            image_dest_rect(area, u32::MAX, u32::MAX, u32::MAX, u32::MAX, u32::MAX),
            body(30, 0, 70, 40)
        );
        // Wider than any u32, one pixel tall: the body width, one row, centred.
        let wide = body(-1_000_000, -2_000, 1_000_000, 3_000);
        assert_eq!(
            image_dest_rect(wide, 1, 1, u32::MAX, 1, u32::MAX),
            body(-1_000_000, 499, 1_000_000, 500)
        );
        let extreme = body(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        let placed = image_dest_rect(extreme, u32::MAX, 1, u32::MAX, u32::MAX, 96);
        assert_eq!(placed.left, i32::MIN);
        assert_eq!(placed.top, i32::MIN);
        assert_eq!(placed.right, -1);
        assert_eq!(placed.bottom, -1);
        assert_eq!(placed.width(), i32::MAX);
        assert_eq!(placed.height(), i32::MAX);
    }

    fn rgb_bmp(width: u32, height: u32, rgb: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        BmpEncoder::new(&mut buf)
            .encode(rgb, width, height, ExtendedColorType::Rgb8)
            .unwrap();
        buf
    }

    fn rgba_webp(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        WebPEncoder::new_lossless(&mut buf)
            .encode(rgba, width, height, ExtendedColorType::Rgba8)
            .unwrap();
        buf
    }

    fn ico_pngs(frames: &[(u32, u32, &[u8])]) -> Vec<u8> {
        let encoded: Vec<_> = frames
            .iter()
            .map(|(width, height, pixels)| {
                IcoFrame::as_png(pixels, *width, *height, ExtendedColorType::Rgba8).unwrap()
            })
            .collect();
        let mut buf = Vec::new();
        IcoEncoder::new(&mut buf).encode_images(&encoded).unwrap();
        buf
    }

    /// `BITMAPINFOHEADER` and no pixels. Width and height are the claimed size.
    fn crafted_bmp(width: i32, height: i32) -> Vec<u8> {
        let dib_size: u32 = 40;
        let off_bits = BMP_FILE_HEADER_LEN + dib_size;
        let mut data = Vec::with_capacity(54);
        data.extend_from_slice(b"BM");
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&off_bits.to_le_bytes());
        data.extend_from_slice(&dib_size.to_le_bytes());
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&24u16.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 20]);
        data
    }

    fn crafted_gif(width: u16, height: u16) -> Vec<u8> {
        let mut data = b"GIF89a".to_vec();
        data.extend_from_slice(&width.to_le_bytes());
        data.extend_from_slice(&height.to_le_bytes());
        // Packed field, background, aspect. No global colour table.
        data.extend_from_slice(&[0x00, 0x00, 0x00]);
        data
    }

    /// `VP8X` canvas and no frame. Width and height are the claimed size.
    fn crafted_webp(width: u32, height: u32) -> Vec<u8> {
        let w = width - 1;
        let h = height - 1;
        let mut chunk = Vec::with_capacity(18);
        chunk.extend_from_slice(b"VP8X");
        chunk.extend_from_slice(&10u32.to_le_bytes());
        chunk.push(0);
        chunk.extend_from_slice(&[0, 0, 0]);
        chunk.push((w & 0xFF) as u8);
        chunk.push(((w >> 8) & 0xFF) as u8);
        chunk.push(((w >> 16) & 0xFF) as u8);
        chunk.push((h & 0xFF) as u8);
        chunk.push(((h >> 8) & 0xFF) as u8);
        chunk.push(((h >> 16) & 0xFF) as u8);
        let mut data = Vec::with_capacity(12 + chunk.len());
        data.extend_from_slice(b"RIFF");
        let size = u32::try_from(4 + chunk.len()).unwrap();
        data.extend_from_slice(&size.to_le_bytes());
        data.extend_from_slice(b"WEBP");
        data.extend_from_slice(&chunk);
        data
    }

    fn animated_gif(first: [u8; 4], second: [u8; 4]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut encoder = GifEncoder::new_with_speed(&mut buf, 30);
            encoder
                .encode_frames([
                    Frame::from_parts(
                        RgbaImage::from_pixel(2, 2, image::Rgba(first)),
                        0,
                        0,
                        Delay::from_numer_denom_ms(50, 1),
                    ),
                    Frame::from_parts(
                        RgbaImage::from_pixel(2, 2, image::Rgba(second)),
                        0,
                        0,
                        Delay::from_numer_denom_ms(50, 1),
                    ),
                ])
                .unwrap();
        }
        buf
    }

    #[test]
    fn sniff_matches_bmp_gif_ico_and_webp_and_rejects_lookalikes() {
        assert_eq!(sniff_image(b"GIF87a"), Some(ImageFormat::Gif));
        assert_eq!(sniff_image(b"GIF89a"), Some(ImageFormat::Gif));
        assert_eq!(sniff_image(b"GIF89a extra"), Some(ImageFormat::Gif));
        assert_eq!(sniff_image(b"GIF89"), None);
        assert_eq!(sniff_image(b"GIF89b"), None);
        assert_eq!(sniff_image(b"GIF87"), None);
        assert_eq!(sniff_image(b"gif89a"), None);

        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&12u32.to_le_bytes());
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_image(&webp), Some(ImageFormat::WebP));
        webp.push(0);
        assert_eq!(sniff_image(&webp), Some(ImageFormat::WebP));
        assert_eq!(sniff_image(b"RIFF"), None);
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEB"), None);
        let mut wave = b"RIFF".to_vec();
        wave.extend_from_slice(&4u32.to_le_bytes());
        wave.extend_from_slice(b"WAVE");
        assert_eq!(sniff_image(&wave), None);
        let mut buried = b"xx".to_vec();
        buried.extend_from_slice(b"RIFF\0\0\0\0WEBP");
        assert_eq!(sniff_image(&buried), None);

        assert_eq!(
            sniff_image(&[0x00, 0x00, 0x01, 0x00, 0x01, 0x00]),
            Some(ImageFormat::Ico)
        );
        assert_eq!(
            sniff_image(&[0x00, 0x00, 0x01, 0x00, 0x02, 0x00, 0xFF]),
            Some(ImageFormat::Ico)
        );
        // Count 0 is an empty directory, not an image.
        assert_eq!(sniff_image(&[0x00, 0x00, 0x01, 0x00, 0x00, 0x00]), None);
        assert_eq!(sniff_image(&[0x00, 0x00, 0x01, 0x00]), None);
        assert_eq!(sniff_image(&[0x00, 0x00, 0x01]), None);
        // CUR is not ICO.
        assert_eq!(sniff_image(&[0x00, 0x00, 0x02, 0x00, 0x01, 0x00]), None);

        assert_eq!(sniff_image(b"BM"), None);
        assert_eq!(sniff_image(b"BMhello world, this is text"), None);
        let bmp = crafted_bmp(2, 2);
        assert_eq!(sniff_image(&bmp), Some(ImageFormat::Bmp));
        assert_eq!(sniff_image(&bmp[..17]), None);
        let mut reserved = bmp.clone();
        reserved[6] = 1;
        assert_eq!(sniff_image(&reserved), None);
        let mut dib = bmp.clone();
        dib[14] = 39;
        assert_eq!(sniff_image(&dib), None);
        let mut early = bmp.clone();
        early[10..14].copy_from_slice(&14u32.to_le_bytes());
        assert_eq!(sniff_image(&early), None);
    }

    #[test]
    fn bmp_pixels_are_exact_bgra() {
        let bmp = rgb_bmp(2, 2, &[10, 20, 30, 10, 20, 30, 10, 20, 30, 10, 20, 30]);
        assert_eq!(sniff_image(&bmp), Some(ImageFormat::Bmp));
        let decoded = decode_image(&bmp, ImageFormat::Bmp, &limits()).unwrap();
        assert_eq!(decoded.format, ImageFormat::Bmp);
        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!((decoded.source_width, decoded.source_height), (2, 2));
        assert_eq!(decoded.bgra, [30, 20, 10, 255].repeat(4));
    }

    #[test]
    fn gif_animation_keeps_only_the_first_frame() {
        let gif = animated_gif([255, 0, 0, 255], [0, 0, 255, 255]);
        assert_eq!(sniff_image(&gif), Some(ImageFormat::Gif));
        // Two image separators: the file really has a second frame.
        assert!(gif.iter().filter(|byte| **byte == 0x2C).count() >= 2);
        let decoded = decode_image(&gif, ImageFormat::Gif, &limits()).unwrap();
        assert_eq!(decoded.format, ImageFormat::Gif);
        assert_eq!((decoded.source_width, decoded.source_height), (2, 2));
        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!(decoded.bgra.len(), 2 * 2 * 4);
        assert!(
            decoded
                .bgra
                .chunks_exact(4)
                .all(|px| px[2] > 200 && px[2] > px[0] && px[2] > px[1]),
            "first frame should stay red, got {:?}",
            decoded.bgra
        );
    }

    #[test]
    fn webp_pixels_are_exact_bgra() {
        let webp = rgba_webp(2, 1, &[0, 0, 255, 255, 0, 0, 255, 128]);
        assert_eq!(sniff_image(&webp), Some(ImageFormat::WebP));
        let decoded = decode_image(&webp, ImageFormat::WebP, &limits()).unwrap();
        assert_eq!(decoded.format, ImageFormat::WebP);
        assert_eq!((decoded.source_width, decoded.source_height), (2, 1));
        assert_eq!((decoded.width, decoded.height), (2, 1));
        // Opaque blue, then blue at alpha 128 over white. Same mix as the PNG path.
        assert_eq!(decoded.bgra, vec![255, 0, 0, 255, 255, 127, 127, 255]);
    }

    #[test]
    fn ico_keeps_the_largest_frame_that_fits() {
        let red = [255u8, 0, 0, 255].repeat(4);
        let blue = [0u8, 0, 255, 255];
        let large_first = ico_pngs(&[(2, 2, &red), (1, 1, &blue)]);
        let small_first = ico_pngs(&[(1, 1, &blue), (2, 2, &red)]);
        assert_eq!(sniff_image(&large_first), Some(ImageFormat::Ico));
        assert_eq!(sniff_image(&small_first), Some(ImageFormat::Ico));
        for bytes in [&large_first, &small_first] {
            let decoded = decode_image(bytes, ImageFormat::Ico, &limits()).unwrap();
            assert_eq!(decoded.format, ImageFormat::Ico);
            assert_eq!((decoded.source_width, decoded.source_height), (2, 2));
            assert_eq!((decoded.width, decoded.height), (2, 2));
            // Red, written BGRA. The 1×1 blue frame was not the one selected.
            assert_eq!(decoded.bgra, [0, 0, 255, 255].repeat(4));
        }

        let by_side = ImageLimits {
            max_dimension: 1,
            ..ImageLimits::default()
        };
        let decoded = decode_image(&large_first, ImageFormat::Ico, &by_side).unwrap();
        assert_eq!((decoded.source_width, decoded.source_height), (1, 1));
        assert_eq!(decoded.bgra, vec![255, 0, 0, 255]);

        let by_pixels = ImageLimits {
            max_pixels: 1,
            ..ImageLimits::default()
        };
        let decoded = decode_image(&small_first, ImageFormat::Ico, &by_pixels).unwrap();
        assert_eq!((decoded.width, decoded.height), (1, 1));
        assert_eq!(decoded.bgra, vec![255, 0, 0, 255]);

        let none = ImageLimits {
            max_pixels: 0,
            ..ImageLimits::default()
        };
        assert_eq!(
            decode_image(&large_first, ImageFormat::Ico, &none).unwrap_err(),
            ImageError::TooLarge {
                width: 1,
                height: 1
            }
        );
        assert_eq!(
            decode_image(&[0, 0, 1, 0, 0, 0], ImageFormat::Ico, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(&[0, 0, 2, 0, 1, 0], ImageFormat::Ico, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
    }

    #[test]
    fn oversized_bmp_gif_and_webp_headers_are_too_large() {
        assert_eq!(
            decode_image(&crafted_bmp(40_000, 2), ImageFormat::Bmp, &limits()).unwrap_err(),
            ImageError::TooLarge {
                width: 40_000,
                height: 2
            }
        );
        assert_eq!(
            decode_image(&crafted_bmp(20_000, 20_000), ImageFormat::Bmp, &limits()).unwrap_err(),
            ImageError::TooLarge {
                width: 20_000,
                height: 20_000
            }
        );
        assert_eq!(
            decode_image(&crafted_gif(40_000, 1), ImageFormat::Gif, &limits()).unwrap_err(),
            ImageError::TooLarge {
                width: 40_000,
                height: 1
            }
        );
        assert_eq!(
            decode_image(&crafted_gif(10_001, 10_000), ImageFormat::Gif, &limits()).unwrap_err(),
            ImageError::TooLarge {
                width: 10_001,
                height: 10_000
            }
        );
        let wide = crafted_webp(40_000, 2);
        assert_eq!(webp_canvas_dimensions(&wide), Some((40_000, 2)));
        assert_eq!(
            decode_image(&wide, ImageFormat::WebP, &limits()).unwrap_err(),
            ImageError::TooLarge {
                width: 40_000,
                height: 2
            }
        );
        assert_eq!(
            decode_image(&crafted_webp(10_001, 10_000), ImageFormat::WebP, &limits()).unwrap_err(),
            ImageError::TooLarge {
                width: 10_001,
                height: 10_000
            }
        );
    }

    #[test]
    fn bmp_geometry_and_alloc_limits_apply_before_pixels_are_stored() {
        let bmp = rgb_bmp(2, 2, &[9, 8, 7, 9, 8, 7, 9, 8, 7, 9, 8, 7]);
        let too_many = ImageLimits {
            max_pixels: 3,
            ..ImageLimits::default()
        };
        assert_eq!(
            decode_image(&bmp, ImageFormat::Bmp, &too_many).unwrap_err(),
            ImageError::TooLarge {
                width: 2,
                height: 2
            }
        );
        let too_wide = ImageLimits {
            max_dimension: 1,
            ..ImageLimits::default()
        };
        assert_eq!(
            decode_image(&bmp, ImageFormat::Bmp, &too_wide).unwrap_err(),
            ImageError::TooLarge {
                width: 2,
                height: 2
            }
        );
        let exact = ImageLimits {
            max_pixels: 4,
            max_dimension: 2,
            ..ImageLimits::default()
        };
        let decoded = decode_image(&bmp, ImageFormat::Bmp, &exact).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 2));

        let bigger = rgb_bmp(16, 16, &vec![40u8; 16 * 16 * 3]);
        let tight = ImageLimits {
            max_alloc_bytes: 32,
            ..ImageLimits::default()
        };
        assert_eq!(
            decode_image(&bigger, ImageFormat::Bmp, &tight).unwrap_err(),
            ImageError::TooLarge {
                width: 16,
                height: 16
            }
        );
    }

    #[test]
    fn header_only_bmp_and_gif_inside_the_limits_are_corrupt() {
        assert_eq!(
            decode_image(&crafted_bmp(1, 1), ImageFormat::Bmp, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(&crafted_bmp(0, 4), ImageFormat::Bmp, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(&crafted_gif(1, 1), ImageFormat::Gif, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(&crafted_gif(0, 4), ImageFormat::Gif, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(b"this is not an image", ImageFormat::Bmp, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
        assert_eq!(
            decode_image(b"this is not an image", ImageFormat::WebP, &limits()).unwrap_err(),
            ImageError::Corrupt
        );
    }
}
