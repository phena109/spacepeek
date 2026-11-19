//! Fit a PDF page into the preview box.
//!
//! `PdfPage.Size` is in device-independent pixels (1/96 inch). DestinationWidth
//! and DestinationHeight are pixels. A scale of 1 is the natural preview size.
//! Pages smaller than the box are not upscaled. Extreme aspect ratios fall
//! back to the full box.

#![cfg_attr(not(windows), allow(dead_code))]

/// Aspect ratios wider or taller than this use the destination box as-is.
const AWKWARD_ASPECT: f32 = 20.0;

pub fn fit_destination(page_w: f32, page_h: f32, box_w: u32, box_h: u32) -> (u32, u32) {
    let box_w = box_w.max(1);
    let box_h = box_h.max(1);
    if !page_w.is_finite() || !page_h.is_finite() || page_w < 1.0 || page_h < 1.0 {
        return (box_w, box_h);
    }
    let aspect = page_w / page_h;
    if !aspect.is_finite() || aspect > AWKWARD_ASPECT || aspect < (1.0 / AWKWARD_ASPECT) {
        return (box_w, box_h);
    }
    let bw = box_w as f32;
    let bh = box_h as f32;
    let scale = (bw / page_w).min(bh / page_h).min(1.0);
    if !scale.is_finite() || scale <= 0.0 {
        return (box_w, box_h);
    }
    let w = (page_w * scale).round().clamp(1.0, bw) as u32;
    let h = (page_h * scale).round().clamp(1.0, bh) as u32;
    if w == 0 || h == 0 {
        (box_w, box_h)
    } else {
        (w, h)
    }
}

#[cfg(test)]
mod tests {
    use super::fit_destination;

    #[test]
    fn letter_dips_are_not_upscaled() {
        // 8.5in x 11in at 96 DPI.
        assert_eq!(fit_destination(816.0, 1056.0, 900, 1200), (816, 1056));
    }

    #[test]
    fn large_page_scales_down_inside_the_box() {
        assert_eq!(fit_destination(2000.0, 3000.0, 900, 1200), (800, 1200));
        assert_eq!(fit_destination(5000.0, 5000.0, 900, 1200), (900, 900));
    }

    #[test]
    fn small_page_is_not_upscaled() {
        assert_eq!(fit_destination(100.0, 100.0, 900, 1200), (100, 100));
    }

    #[test]
    fn awkward_aspect_uses_the_box() {
        assert_eq!(fit_destination(10_000.0, 100.0, 900, 1200), (900, 1200));
        assert_eq!(fit_destination(100.0, 10_000.0, 900, 1200), (900, 1200));
    }

    #[test]
    fn invalid_page_size_uses_the_box() {
        assert_eq!(fit_destination(0.0, 100.0, 900, 1200), (900, 1200));
        assert_eq!(fit_destination(f32::NAN, 100.0, 900, 1200), (900, 1200));
        assert_eq!(
            fit_destination(f32::INFINITY, 100.0, 900, 1200),
            (900, 1200)
        );
    }
}
