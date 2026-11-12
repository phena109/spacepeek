//! Zoom and pan for one still image. No pixels are decoded here.
//!
//! The scale is a per-mille of the **fit-to-window** size ([`ZOOM_FIT_MILLI`]
//! is fit). Fit is sticky: resizing the window recomputes [`image_dest_rect`]
//! and the picture stays fitted. A manual zoom keeps its per-mille of the
//! *current* fit, so a resize does not snap back to fit. The pan is clamped
//! into the new body.
//!
//! The range is 10%–800% of that fit size ([`ZOOM_MIN_MILLI`]–[`ZOOM_MAX_MILLI`]).
//! One wheel notch or one `+` / `-` multiplies by 5/4 (in) or 4/5 (out).
//! Actual size (`1`) is the DPI-scaled source, the same size [`image_dest_rect`]
//! starts from, clamped into that range and centred.
//!
//! Left-drag versus window chrome (the window procedure applies this; the
//! maths here only pans when [`ImageView::can_pan`] is true):
//! - Header (`HTCAPTION`): drag the window. This module is not involved.
//! - Close button: a press on × does not pan. Button-up still closes.
//! - Body, zoomed past fit: left-drag pans the image.
//! - Body, at fit or zoomed out to fit or smaller: left-drag does nothing.
//!   It does not move the window and it does not pan.
//!
//! Pan is clamped so the picture cannot leave the body. When the zoomed image
//! is larger than the body on an axis, that axis stays covered (no empty gap).
//! When it is smaller, the image stays fully inside the body. Below or at fit,
//! the pan is zero and the image stays centred. Arrow keys are
//! [`ImageCommand::Navigate`] and do not change this state.

use crate::geometry::{effective_dpi, PxRect, BASE_DPI};
use crate::image_preview::image_dest_rect;

/// Fit to the window. 100% of the fitted destination.
pub const ZOOM_FIT_MILLI: u32 = 1_000;
/// Smallest manual zoom: 10% of the fitted destination.
pub const ZOOM_MIN_MILLI: u32 = 100;
/// Largest manual zoom: 800% of the fitted destination.
pub const ZOOM_MAX_MILLI: u32 = 8_000;
/// One zoom-in step is `* ZOOM_STEP_NUM / ZOOM_STEP_DEN` (25%).
pub const ZOOM_STEP_NUM: u32 = 5;
pub const ZOOM_STEP_DEN: u32 = 4;
/// Cap on how many steps one wheel message or one call applies.
const MAX_ZOOM_STEPS: u32 = 64;

/// Where the bitmap is drawn, plus the zoom policy for the next resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageView {
    /// Per-mille of the current fit size. Ignored while [`Self::fitted`] is set.
    zoom_milli: u32,
    /// Set for a fresh image, `0`, and a zoom step that lands on fit.
    fitted: bool,
    /// Added to the centred origin. Positive moves the image right and down.
    pan_x: i32,
    pan_y: i32,
}

impl Default for ImageView {
    fn default() -> Self {
        Self::fit()
    }
}

impl ImageView {
    pub const fn fit() -> Self {
        Self {
            zoom_milli: ZOOM_FIT_MILLI,
            fitted: true,
            pan_x: 0,
            pan_y: 0,
        }
    }

    pub const fn is_fitted(self) -> bool {
        self.fitted
    }

    /// Effective scale. A fitted view is [`ZOOM_FIT_MILLI`] even if a stale
    /// value were stored.
    pub const fn zoom_milli(self) -> u32 {
        if self.fitted {
            ZOOM_FIT_MILLI
        } else {
            self.zoom_milli
        }
    }

    /// Body left-drag pans only while the scale is past fit-to-window.
    pub const fn can_pan(self) -> bool {
        !self.fitted && self.zoom_milli > ZOOM_FIT_MILLI
    }

    /// Pan in pixels. Zero unless [`Self::can_pan`].
    pub const fn pan(self) -> (i32, i32) {
        if self.can_pan() {
            (self.pan_x, self.pan_y)
        } else {
            (0, 0)
        }
    }
}

/// A new file, or the window closing. Drops whatever zoom the previous image had.
pub const fn reset_for_new_image(_view: ImageView) -> ImageView {
    ImageView::fit()
}

/// Body and bitmap sizes the placement maths needs. `image_*` is the stored
/// buffer; `source_*` is the file size before any downscale, which is what
/// fit and actual size use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageFrame {
    pub body: PxRect,
    pub image_w: u32,
    pub image_h: u32,
    pub source_w: u32,
    pub source_h: u32,
    pub dpi: u32,
}

/// What can change the view. [`Self::Navigate`] is the arrow keys: they move
/// the Explorer selection and must leave zoom and pan alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageCommand {
    ZoomIn {
        steps: u32,
    },
    ZoomOut {
        steps: u32,
    },
    Fit,
    Actual,
    /// `origin_pan` is the pan at mouse-down. `delta` is the pointer's move
    /// since then, in the same pixels as [`ImageFrame::body`].
    Pan {
        origin_pan: (i32, i32),
        delta: (i32, i32),
    },
    Navigate,
}

/// Positive `delta` (wheel away from the user) zooms in. Notches are
/// `delta / 120`, same truncation as text scrolling, capped at
/// [`MAX_ZOOM_STEPS`]. A partial notch is `None`.
pub fn wheel_zoom_command(delta: i32) -> Option<ImageCommand> {
    let steps = delta / 120;
    if steps > 0 {
        Some(ImageCommand::ZoomIn {
            steps: cap_steps(steps),
        })
    } else if steps < 0 {
        Some(ImageCommand::ZoomOut {
            steps: cap_steps_unsigned(steps.unsigned_abs()),
        })
    } else {
        None
    }
}

fn cap_steps(steps: i32) -> u32 {
    u32::try_from(steps).unwrap_or(u32::MAX).min(MAX_ZOOM_STEPS)
}

fn cap_steps_unsigned(steps: u32) -> u32 {
    steps.min(MAX_ZOOM_STEPS)
}

/// Apply `command`. `cursor` is in the same coordinates as the body. A point
/// inside the body zooms around that point; anything else, including `None`,
/// zooms around the body centre.
pub fn apply_image_command(
    view: ImageView,
    frame: ImageFrame,
    command: ImageCommand,
    cursor: Option<(i32, i32)>,
) -> ImageView {
    match command {
        ImageCommand::Navigate => view,
        ImageCommand::Fit => ImageView::fit(),
        ImageCommand::Actual => actual_view(frame),
        ImageCommand::ZoomIn { steps } => repeat_zoom(view, frame, steps, true, cursor),
        ImageCommand::ZoomOut { steps } => repeat_zoom(view, frame, steps, false, cursor),
        ImageCommand::Pan { origin_pan, delta } => pan_view(view, frame, origin_pan, delta),
    }
}

/// Clamp pan for the current policy. Fit stays fit. A manual scale is kept.
pub fn reconcile_image_view(view: ImageView, frame: ImageFrame) -> ImageView {
    if view.fitted {
        return ImageView::fit();
    }
    let milli = view.zoom_milli.clamp(ZOOM_MIN_MILLI, ZOOM_MAX_MILLI);
    if milli <= ZOOM_FIT_MILLI {
        return ImageView {
            zoom_milli: milli,
            fitted: milli == ZOOM_FIT_MILLI,
            pan_x: 0,
            pan_y: 0,
        };
    }
    let fit = fitted_rect(frame);
    if fit.width() == 0 || fit.height() == 0 {
        return ImageView {
            zoom_milli: milli,
            fitted: false,
            pan_x: 0,
            pan_y: 0,
        };
    }
    let size = scaled_size(fit, milli);
    let centred = centred_origin(frame.body, size);
    let (pan_x, pan_y) = clamp_pan(frame.body, centred, size, (view.pan_x, view.pan_y));
    ImageView {
        zoom_milli: milli,
        fitted: false,
        pan_x,
        pan_y,
    }
}

/// Destination used by `StretchDIBits`. A fitted view is exactly
/// [`image_dest_rect`].
pub fn image_view_rect(frame: ImageFrame, view: ImageView) -> PxRect {
    let fit = fitted_rect(frame);
    if view.is_fitted() || fit.width() == 0 || fit.height() == 0 {
        return fit;
    }
    let placed = reconcile_image_view(view, frame);
    if placed.is_fitted() {
        return fit;
    }
    let size = scaled_size(fit, placed.zoom_milli());
    let centred = centred_origin(frame.body, size);
    let left = add_i32(centred.0, placed.pan_x);
    let top = add_i32(centred.1, placed.pan_y);
    PxRect {
        left,
        top,
        right: add_i32(left, size.0),
        bottom: add_i32(top, size.1),
    }
}

/// Pack a screen point into one 64-bit word. Both axes keep the full `i32`
/// range, including negatives and values past 16 bits.
pub fn pack_screen_point(x: i32, y: i32) -> i64 {
    let x = u64::from(x as u32);
    let y = u64::from(y as u32);
    (x | (y << 32)) as i64
}

pub fn unpack_screen_point(packed: i64) -> (i32, i32) {
    let bits = packed as u64;
    let x = bits as u32 as i32;
    let y = (bits >> 32) as u32 as i32;
    (x, y)
}

fn repeat_zoom(
    mut view: ImageView,
    frame: ImageFrame,
    steps: u32,
    zoom_in: bool,
    cursor: Option<(i32, i32)>,
) -> ImageView {
    let steps = cap_steps_unsigned(steps);
    for _ in 0..steps {
        let next = step_zoom(view, frame, zoom_in, cursor);
        if next == view {
            break;
        }
        view = next;
    }
    view
}

fn step_zoom(
    view: ImageView,
    frame: ImageFrame,
    zoom_in: bool,
    cursor: Option<(i32, i32)>,
) -> ImageView {
    let current = view.zoom_milli();
    let next = step_milli(current, zoom_in);
    if next == current {
        return reconcile_image_view(view, frame);
    }
    // At or below fit the picture is centred. Cursor anchoring applies only
    // while the result is past fit, which is also when a drag can pan.
    if next <= ZOOM_FIT_MILLI {
        return ImageView {
            zoom_milli: next,
            fitted: next == ZOOM_FIT_MILLI,
            pan_x: 0,
            pan_y: 0,
        };
    }
    let old = image_view_rect(frame, view);
    if old.width() == 0 || old.height() == 0 {
        return ImageView::fit();
    }
    let fit = fitted_rect(frame);
    let size = scaled_size(fit, next);
    let anchor = zoom_anchor(frame.body, cursor);
    let new_left = anchor_origin(old.left, old.width(), size.0, anchor.0);
    let new_top = anchor_origin(old.top, old.height(), size.1, anchor.1);
    let centred = centred_origin(frame.body, size);
    let view = ImageView {
        zoom_milli: next,
        fitted: false,
        pan_x: sub_i32(new_left, centred.0),
        pan_y: sub_i32(new_top, centred.1),
    };
    reconcile_image_view(view, frame)
}

fn step_milli(milli: u32, zoom_in: bool) -> u32 {
    let milli = milli.clamp(ZOOM_MIN_MILLI, ZOOM_MAX_MILLI);
    let next = if zoom_in {
        u64::from(milli) * u64::from(ZOOM_STEP_NUM) / u64::from(ZOOM_STEP_DEN)
    } else {
        u64::from(milli) * u64::from(ZOOM_STEP_DEN) / u64::from(ZOOM_STEP_NUM)
    };
    let next = u32::try_from(next)
        .unwrap_or(ZOOM_MAX_MILLI)
        .clamp(ZOOM_MIN_MILLI, ZOOM_MAX_MILLI);
    if next != milli {
        return next;
    }
    // The fraction did not move (only happens at a clamp, for these constants).
    // Nudge by one per-mille when there is still room so a step cannot stick
    // in the middle of the range.
    if zoom_in && milli < ZOOM_MAX_MILLI {
        milli + 1
    } else if !zoom_in && milli > ZOOM_MIN_MILLI {
        milli - 1
    } else {
        milli
    }
}

fn actual_view(frame: ImageFrame) -> ImageView {
    let fit = fitted_rect(frame);
    let fit_w = fit.width();
    let fit_h = fit.height();
    if fit_w == 0 || fit_h == 0 {
        return ImageView::fit();
    }
    let (desired_w, desired_h) = dpi_source_px(frame.source_w, frame.source_h, frame.dpi);
    if desired_w <= 0 || desired_h <= 0 {
        return ImageView::fit();
    }
    // Width and height can disagree by a pixel after `fit_size` rounds.
    // The larger ratio keeps neither axis short of the DPI-scaled source.
    let milli = ratio_milli(desired_w, fit_w)
        .max(ratio_milli(desired_h, fit_h))
        .clamp(ZOOM_MIN_MILLI, ZOOM_MAX_MILLI);
    if milli <= ZOOM_FIT_MILLI {
        // The fitted picture is already at actual size (the source is smaller
        // than the body, so fit does not upscale). Stay on the fit policy.
        return ImageView::fit();
    }
    reconcile_image_view(
        ImageView {
            zoom_milli: milli,
            fitted: false,
            pan_x: 0,
            pan_y: 0,
        },
        frame,
    )
}

fn pan_view(
    view: ImageView,
    frame: ImageFrame,
    origin_pan: (i32, i32),
    delta: (i32, i32),
) -> ImageView {
    if !view.can_pan() {
        return view;
    }
    reconcile_image_view(
        ImageView {
            pan_x: origin_pan.0.saturating_add(delta.0),
            pan_y: origin_pan.1.saturating_add(delta.1),
            ..view
        },
        frame,
    )
}

fn fitted_rect(frame: ImageFrame) -> PxRect {
    image_dest_rect(
        frame.body,
        frame.image_w,
        frame.image_h,
        frame.source_w,
        frame.source_h,
        frame.dpi,
    )
}

/// DPI-scaled source size. Same truncating `source * dpi / 96` rule as
/// [`image_dest_rect`], which then fits that box into the body.
fn dpi_source_px(source_w: u32, source_h: u32, dpi: u32) -> (i32, i32) {
    let dpi = u64::from(effective_dpi(dpi));
    let base = u64::from(u32::try_from(BASE_DPI).unwrap_or(96));
    let desired_w = u64::from(source_w).saturating_mul(dpi) / base;
    let desired_h = u64::from(source_h).saturating_mul(dpi) / base;
    (clamp_i32_u64(desired_w), clamp_i32_u64(desired_h))
}

fn ratio_milli(desired: i32, fit: i32) -> u32 {
    if desired <= 0 || fit <= 0 {
        return ZOOM_FIT_MILLI;
    }
    let milli = (i64::from(desired).saturating_mul(1_000) + i64::from(fit) / 2) / i64::from(fit);
    u32::try_from(milli.max(0)).unwrap_or(ZOOM_MAX_MILLI)
}

fn scaled_size(fit: PxRect, milli: u32) -> (i32, i32) {
    (
        scaled_side(fit.width(), milli),
        scaled_side(fit.height(), milli),
    )
}

fn scaled_side(fit: i32, milli: u32) -> i32 {
    if fit <= 0 {
        return 0;
    }
    let scaled = i64::from(fit).saturating_mul(i64::from(milli)) / 1_000;
    clamp_i32(scaled).max(1)
}

fn centred_origin(body: PxRect, size: (i32, i32)) -> (i32, i32) {
    let left = i64::from(body.left) + (i64::from(body.width()) - i64::from(size.0)) / 2;
    let top = i64::from(body.top) + (i64::from(body.height()) - i64::from(size.1)) / 2;
    (clamp_i32(left), clamp_i32(top))
}

fn zoom_anchor(body: PxRect, cursor: Option<(i32, i32)>) -> (i32, i32) {
    if let Some((x, y)) = cursor {
        if x >= body.left && x < body.right && y >= body.top && y < body.bottom {
            return (x, y);
        }
    }
    (
        clamp_i32(i64::from(body.left) + i64::from(body.width()) / 2),
        clamp_i32(i64::from(body.top) + i64::from(body.height()) / 2),
    )
}

/// `new_origin` so the image point under `anchor` stays under `anchor`.
fn anchor_origin(old_origin: i32, old_size: i32, new_size: i32, anchor: i32) -> i32 {
    if old_size <= 0 {
        return old_origin;
    }
    let rel = i64::from(anchor) - i64::from(old_origin);
    let shifted = rel.saturating_mul(i64::from(new_size)) / i64::from(old_size);
    clamp_i32(i64::from(anchor).saturating_sub(shifted))
}

/// Larger than the body: cover it. Smaller: stay fully inside. Either way the
/// image cannot be dragged completely out.
fn clamp_pan(body: PxRect, centred: (i32, i32), size: (i32, i32), pan: (i32, i32)) -> (i32, i32) {
    (
        clamp_pan_axis(body.left, body.width(), centred.0, size.0, pan.0),
        clamp_pan_axis(body.top, body.height(), centred.1, size.1, pan.1),
    )
}

fn clamp_pan_axis(origin: i32, span: i32, centred: i32, size: i32, pan: i32) -> i32 {
    if span <= 0 || size <= 0 {
        return 0;
    }
    let origin = i64::from(origin);
    let end = origin + i64::from(span);
    let size = i64::from(size);
    let centred = i64::from(centred);
    let (min_left, max_left) = if size >= i64::from(span) {
        (end - size, origin)
    } else {
        (origin, end - size)
    };
    let left = (centred + i64::from(pan)).clamp(min_left, max_left);
    clamp_i32(left - centred)
}

fn add_i32(a: i32, b: i32) -> i32 {
    clamp_i32(i64::from(a).saturating_add(i64::from(b)))
}

fn sub_i32(a: i32, b: i32) -> i32 {
    clamp_i32(i64::from(a).saturating_sub(i64::from(b)))
}

fn clamp_i32(value: i64) -> i32 {
    if value > i64::from(i32::MAX) {
        i32::MAX
    } else if value < i64::from(i32::MIN) {
        i32::MIN
    } else {
        value as i32
    }
}

fn clamp_i32_u64(value: u64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_frame(body: PxRect, source_w: u32, source_h: u32, dpi: u32) -> ImageFrame {
        ImageFrame {
            body,
            image_w: source_w,
            image_h: source_h,
            source_w,
            source_h,
            dpi,
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

    fn square() -> ImageFrame {
        make_frame(body(0, 0, 200, 200), 200, 200, 96)
    }

    fn apply(
        view: ImageView,
        frame: ImageFrame,
        command: ImageCommand,
        cursor: Option<(i32, i32)>,
    ) -> ImageView {
        apply_image_command(view, frame, command, cursor)
    }

    #[test]
    fn fitted_view_matches_image_dest_rect() {
        let samples = [
            square(),
            make_frame(body(10, 20, 210, 120), 100, 200, 96),
            make_frame(body(0, 0, 500, 500), 10, 8, 192),
            make_frame(body(-20, -10, 180, 90), 100, 50, 96),
            make_frame(body(0, 0, 100, 80), 200, 100, 192),
        ];
        for frame in samples {
            let view = ImageView::fit();
            assert!(view.is_fitted());
            assert_eq!(view.zoom_milli(), ZOOM_FIT_MILLI);
            assert!(!view.can_pan());
            assert_eq!(
                image_view_rect(frame, view),
                image_dest_rect(
                    frame.body,
                    frame.image_w,
                    frame.image_h,
                    frame.source_w,
                    frame.source_h,
                    frame.dpi,
                )
            );
        }
    }

    #[test]
    fn zoom_steps_and_clamp() {
        let frame = square();
        let once = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomIn { steps: 1 },
            None,
        );
        assert_eq!(once.zoom_milli(), 1_250);
        assert!(!once.is_fitted());
        assert!(once.can_pan());
        assert_eq!(image_view_rect(frame, once).width(), 250);

        let back = apply(once, frame, ImageCommand::ZoomOut { steps: 1 }, None);
        assert!(back.is_fitted());
        assert_eq!(back.zoom_milli(), ZOOM_FIT_MILLI);
        assert_eq!(back.pan(), (0, 0));

        let out = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomOut { steps: 1 },
            None,
        );
        assert_eq!(out.zoom_milli(), 800);
        assert!(!out.is_fitted());
        assert!(!out.can_pan());
        assert_eq!(image_view_rect(frame, out).width(), 160);
        let restored = apply(out, frame, ImageCommand::ZoomIn { steps: 1 }, None);
        assert!(restored.is_fitted());

        let maxed = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomIn { steps: u32::MAX },
            None,
        );
        assert_eq!(maxed.zoom_milli(), ZOOM_MAX_MILLI);
        let held = apply(maxed, frame, ImageCommand::ZoomIn { steps: 3 }, None);
        assert_eq!(held.zoom_milli(), ZOOM_MAX_MILLI);
        // 800% of the 200px fit.
        assert_eq!(image_view_rect(frame, maxed).width(), 1_600);

        let mined = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomOut { steps: u32::MAX },
            None,
        );
        assert_eq!(mined.zoom_milli(), ZOOM_MIN_MILLI);
        let held_out = apply(mined, frame, ImageCommand::ZoomOut { steps: 1 }, None);
        assert_eq!(held_out, mined);
        assert_eq!(image_view_rect(frame, mined).width(), 20);
        assert_eq!(
            apply(mined, frame, ImageCommand::ZoomIn { steps: 0 }, None),
            mined
        );
    }

    #[test]
    fn zero_resets_to_fit_and_one_is_actual_size() {
        let frame = square();
        let zoomed = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomIn { steps: 2 },
            None,
        );
        let panned = apply(
            zoomed,
            frame,
            ImageCommand::Pan {
                origin_pan: zoomed.pan(),
                delta: (30, -10),
            },
            None,
        );
        assert_ne!(panned.pan(), (0, 0));
        let fitted = apply(panned, frame, ImageCommand::Fit, None);
        assert_eq!(fitted, ImageView::fit());
        assert_eq!(
            image_view_rect(frame, fitted),
            image_dest_rect(frame.body, 200, 200, 200, 200, 96)
        );

        // Source already fits in the body, so actual size is the fit policy.
        let small = make_frame(body(0, 0, 500, 500), 10, 8, 96);
        let actual_small = apply(panned, small, ImageCommand::Actual, None);
        assert!(actual_small.is_fitted());
        assert_eq!(
            image_view_rect(small, actual_small),
            image_dest_rect(small.body, 10, 8, 10, 8, 96)
        );

        // 400×200 in a 100×100 body. Fit is 100×50. Actual size is 400×200, centred.
        let large = make_frame(body(0, 0, 100, 100), 400, 200, 96);
        assert_eq!(
            image_dest_rect(large.body, 400, 200, 400, 200, 96),
            body(0, 25, 100, 75)
        );
        let actual = apply(ImageView::fit(), large, ImageCommand::Actual, None);
        assert_eq!(actual.zoom_milli(), 4_000);
        assert!(actual.can_pan());
        assert_eq!(actual.pan(), (0, 0));
        let rect = image_view_rect(large, actual);
        assert_eq!(rect, body(-150, -50, 250, 150));
        assert_eq!(rect.width(), 400);
        assert_eq!(rect.height(), 200);

        // 192 DPI: source 200×100 wants 400×200, body 100×80 fits as 100×50.
        let hidpi = make_frame(body(0, 0, 100, 80), 200, 100, 192);
        let hidpi_actual = apply(ImageView::fit(), hidpi, ImageCommand::Actual, None);
        assert_eq!(hidpi_actual.zoom_milli(), 4_000);
        assert_eq!(
            image_view_rect(hidpi, hidpi_actual),
            body(-150, -60, 250, 140)
        );

        // Actual size past 800% of fit clamps. 2000px in a 100px body is 20×.
        let huge = make_frame(body(0, 0, 100, 100), 2_000, 2_000, 96);
        let clamped = apply(ImageView::fit(), huge, ImageCommand::Actual, None);
        assert_eq!(clamped.zoom_milli(), ZOOM_MAX_MILLI);
        let clamped_rect = image_view_rect(huge, clamped);
        assert_eq!(clamped_rect.width(), 800);
        assert_eq!(clamped_rect, body(-350, -350, 450, 450));
    }

    #[test]
    fn cursor_centred_zoom_keeps_the_point_under_the_cursor() {
        let frame = square();
        let cursor = (40, 80);
        let zoomed = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomIn { steps: 1 },
            Some(cursor),
        );
        // 200 → 250. Anchor (40, 80) stays put: left = 40 - 50 = -10, top = 80 - 100 = -20.
        assert_eq!(image_view_rect(frame, zoomed), body(-10, -20, 240, 230));
        assert_eq!(zoomed.pan(), (15, 5));

        let centre = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomIn { steps: 1 },
            Some((100, 100)),
        );
        assert_eq!(centre.pan(), (0, 0));
        assert_eq!(image_view_rect(frame, centre), body(-25, -25, 225, 225));

        // Outside the body, and a missing cursor, both use the body centre.
        let outside = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomIn { steps: 1 },
            Some((500, -4)),
        );
        let missing = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomIn { steps: 1 },
            None,
        );
        assert_eq!(outside, centre);
        assert_eq!(missing, centre);

        let mut view = zoomed;
        for _ in 0..4 {
            let before = image_view_rect(frame, view);
            view = apply(view, frame, ImageCommand::ZoomIn { steps: 1 }, Some(cursor));
            let after = image_view_rect(frame, view);
            assert_anchor_held(before, after, cursor);
        }
    }

    fn assert_anchor_held(before: PxRect, after: PxRect, anchor: (i32, i32)) {
        let err_x = i64::from(anchor.0 - before.left) * i64::from(after.width())
            - i64::from(anchor.0 - after.left) * i64::from(before.width());
        let err_y = i64::from(anchor.1 - before.top) * i64::from(after.height())
            - i64::from(anchor.1 - after.top) * i64::from(before.height());
        assert!(
            err_x.abs() < i64::from(before.width()).max(1),
            "x drift {err_x} before {before:?} after {after:?}"
        );
        assert!(
            err_y.abs() < i64::from(before.height()).max(1),
            "y drift {err_y} before {before:?} after {after:?}"
        );
    }

    #[test]
    fn pan_is_clamped_and_disabled_at_fit() {
        let frame = square();
        let fitted = ImageView::fit();
        let ignored = apply(
            fitted,
            frame,
            ImageCommand::Pan {
                origin_pan: (0, 0),
                delta: (40, 40),
            },
            None,
        );
        assert_eq!(ignored, fitted);

        let zoomed = apply(fitted, frame, ImageCommand::ZoomIn { steps: 1 }, None);
        assert!(zoomed.can_pan());
        let moved = apply(
            zoomed,
            frame,
            ImageCommand::Pan {
                origin_pan: zoomed.pan(),
                delta: (10, -6),
            },
            None,
        );
        assert_eq!(moved.pan(), (10, -6));
        assert_eq!(moved.zoom_milli(), zoomed.zoom_milli());

        // 250px image in a 200px body. Cover clamp stops a huge drag.
        let dragged = apply(
            zoomed,
            frame,
            ImageCommand::Pan {
                origin_pan: (0, 0),
                delta: (100_000, -100_000),
            },
            None,
        );
        let rect = image_view_rect(frame, dragged);
        assert!(rect.left <= 0 && rect.right >= 200);
        assert!(rect.top <= 0 && rect.bottom >= 200);
        let stuck = apply(
            dragged,
            frame,
            ImageCommand::Pan {
                origin_pan: dragged.pan(),
                delta: (100_000, -100_000),
            },
            None,
        );
        assert_eq!(image_view_rect(frame, stuck), rect);

        // Zoomed past fit but still smaller than the body: the image stays inside.
        let small = make_frame(body(0, 0, 400, 400), 20, 20, 96);
        let enlarged = apply(
            ImageView::fit(),
            small,
            ImageCommand::ZoomIn { steps: u32::MAX },
            None,
        );
        assert!(enlarged.can_pan());
        assert!(image_view_rect(small, enlarged).width() < 400);
        let shoved = apply(
            enlarged,
            small,
            ImageCommand::Pan {
                origin_pan: (0, 0),
                delta: (10_000, 10_000),
            },
            None,
        );
        let inside = image_view_rect(small, shoved);
        assert!(inside.left >= 0 && inside.top >= 0);
        assert!(inside.right <= 400 && inside.bottom <= 400);

        // Zooming back to fit drops the pan.
        let cleared = apply(
            dragged,
            frame,
            ImageCommand::ZoomOut { steps: 1 },
            Some((1, 1)),
        );
        assert!(cleared.is_fitted());
        assert_eq!(cleared.pan(), (0, 0));
    }

    #[test]
    fn resize_keeps_fit_and_keeps_a_manual_scale() {
        let small = make_frame(body(0, 0, 100, 100), 400, 200, 96);
        let big = make_frame(body(0, 0, 300, 200), 400, 200, 96);
        let fitted = ImageView::fit();
        assert_eq!(
            image_view_rect(small, fitted),
            image_dest_rect(small.body, 400, 200, 400, 200, 96)
        );
        assert_eq!(
            image_view_rect(big, reconcile_image_view(fitted, big)),
            image_dest_rect(big.body, 400, 200, 400, 200, 96)
        );

        let manual = apply(fitted, small, ImageCommand::ZoomIn { steps: 2 }, None);
        let milli = manual.zoom_milli();
        assert!(milli > ZOOM_FIT_MILLI);
        let reconciled = reconcile_image_view(manual, big);
        assert_eq!(reconciled.zoom_milli(), milli);
        assert!(!reconciled.is_fitted());
        let shown = image_view_rect(big, reconciled);
        let fit_big = image_dest_rect(big.body, 400, 200, 400, 200, 96);
        assert_ne!(shown, fit_big);
        assert_eq!(
            i64::from(shown.width()),
            i64::from(fit_big.width()) * i64::from(milli) / 1_000
        );

        // A pan that was legal in the large body is pulled back when it shrinks.
        let panned = apply(
            reconciled,
            big,
            ImageCommand::Pan {
                origin_pan: (0, 0),
                delta: (80, 40),
            },
            None,
        );
        let shrunk = reconcile_image_view(panned, small);
        assert_eq!(shrunk.zoom_milli(), milli);
        assert_pan_keeps_image_in_body(small.body, image_view_rect(small, shrunk));
    }

    fn assert_pan_keeps_image_in_body(body: PxRect, rect: PxRect) {
        if rect.width() >= body.width() {
            assert!(rect.left <= body.left && rect.right >= body.right);
        } else {
            assert!(rect.left >= body.left && rect.right <= body.right);
        }
        if rect.height() >= body.height() {
            assert!(rect.top <= body.top && rect.bottom >= body.bottom);
        } else {
            assert!(rect.top >= body.top && rect.bottom <= body.bottom);
        }
    }

    #[test]
    fn new_image_resets_zoom_and_pan() {
        let frame = square();
        let zoomed = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomIn { steps: 2 },
            Some((10, 10)),
        );
        let panned = apply(
            zoomed,
            frame,
            ImageCommand::Pan {
                origin_pan: zoomed.pan(),
                delta: (12, 4),
            },
            None,
        );
        let reset = reset_for_new_image(panned);
        assert_eq!(reset, ImageView::fit());
        assert_eq!(
            image_view_rect(frame, reset),
            image_dest_rect(frame.body, 200, 200, 200, 200, 96)
        );
    }

    #[test]
    fn arrows_do_not_change_zoom_state() {
        let frame = square();
        let zoomed = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomIn { steps: 2 },
            Some((20, 40)),
        );
        let panned = apply(
            zoomed,
            frame,
            ImageCommand::Pan {
                origin_pan: zoomed.pan(),
                delta: (8, -3),
            },
            None,
        );
        assert_eq!(
            apply(panned, frame, ImageCommand::Navigate, Some((1, 1))),
            panned
        );
        assert_eq!(panned.zoom_milli(), zoomed.zoom_milli());
    }

    #[test]
    fn wheel_delta_maps_to_zoom_steps() {
        assert_eq!(
            wheel_zoom_command(120),
            Some(ImageCommand::ZoomIn { steps: 1 })
        );
        assert_eq!(
            wheel_zoom_command(-240),
            Some(ImageCommand::ZoomOut { steps: 2 })
        );
        assert_eq!(wheel_zoom_command(60), None);
        assert_eq!(wheel_zoom_command(0), None);
        assert_eq!(
            wheel_zoom_command(i32::MAX),
            Some(ImageCommand::ZoomIn {
                steps: MAX_ZOOM_STEPS
            })
        );
        assert_eq!(
            wheel_zoom_command(i32::MIN),
            Some(ImageCommand::ZoomOut {
                steps: MAX_ZOOM_STEPS
            })
        );
    }

    #[test]
    fn screen_point_packing_keeps_negatives_and_wide_coordinates() {
        for point in [
            (0, 0),
            (-1, -2),
            (40_000, -30_000),
            (i32::MAX, i32::MIN),
            (i32::MIN, i32::MAX),
        ] {
            assert_eq!(
                unpack_screen_point(pack_screen_point(point.0, point.1)),
                point
            );
        }
    }

    #[test]
    fn huge_frame_does_not_overflow() {
        let frame = ImageFrame {
            body: body(0, 0, 1_000_000, 800_000),
            image_w: u32::MAX,
            image_h: u32::MAX,
            source_w: u32::MAX,
            source_h: u32::MAX,
            dpi: u32::MAX,
        };
        let view = apply(
            ImageView::fit(),
            frame,
            ImageCommand::ZoomIn { steps: 8 },
            Some((10, 10)),
        );
        let rect = image_view_rect(frame, view);
        assert!(rect.width() > 0 && rect.height() > 0);
        assert!(rect.right >= rect.left && rect.bottom >= rect.top);
        let actual = apply(view, frame, ImageCommand::Actual, None);
        let actual_rect = image_view_rect(frame, actual);
        assert!(actual_rect.width() > 0 && actual_rect.height() > 0);
        assert!(actual.zoom_milli() <= ZOOM_MAX_MILLI);
    }
}
