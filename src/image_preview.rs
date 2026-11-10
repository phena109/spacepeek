//! PNG and JPEG preview that does not touch Win32.
//!
//! Magic bytes decide the format. The extension is ignored, so a `.txt` that
//! is a PNG decodes and a `.png` that is text does not. Limits are applied
//! before pixel buffers are allocated.

use std::io::Cursor;

use crate::geometry::{effective_dpi, PxRect, BASE_DPI};
use crate::preview::{format_compact_size, format_grouped};

/// Image kinds this previewer decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
}

const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
const JPEG_MAGIC: [u8; 3] = [0xFF, 0xD8, 0xFF];

/// `Some` when `header` starts with a PNG or JPEG signature.
///
/// Short buffers and anything else are `None`. Bytes after the signature are
/// ignored, so a longer file still matches.
pub fn sniff_image(header: &[u8]) -> Option<ImageFormat> {
    if header.starts_with(&PNG_MAGIC) {
        Some(ImageFormat::Png)
    } else if header.starts_with(&JPEG_MAGIC) {
        Some(ImageFormat::Jpeg)
    } else {
        None
    }
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
    /// The bytes are not a decodable PNG or JPEG.
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
                "This image file looks damaged or isn't a supported PNG or JPEG.".to_string()
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
pub fn decode_image(
    bytes: &[u8],
    format: ImageFormat,
    limits: &ImageLimits,
) -> Result<DecodedImage, ImageError> {
    let (width, height) = probe_dimensions(bytes, format)?;
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
/// [`image::ImageReader::into_dimensions`] is the normal path. A PNG whose
/// `IHDR` is the whole file never gets that far: the PNG decoder only reports
/// a size after it has seen the start of `IDAT`. The chunk is then read here
/// so an oversized claim is still [`ImageError::TooLarge`] and is not decoded.
fn probe_dimensions(bytes: &[u8], format: ImageFormat) -> Result<(u32, u32), ImageError> {
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
    }
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
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::PngEncoder;
    use image::{ExtendedColorType, ImageEncoder};

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

        let damaged = "This image file looks damaged or isn't a supported PNG or JPEG.";
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
}
