//! When a settled zoom should re-rasterise an SVG, and at what pixel size.
//!
//! Wheel and keyboard zoom keep the bitmap already on screen and only change
//! [`ImageView`]. This module does not decode, parse, or allocate a pixmap.
//! After [`SVG_ZOOM_SETTLE_MS`] with no further zoom change, and only while
//! the pointer is not dragging the picture or the window border, it may ask a
//! worker to render. The worker is latest-wins: a newer target supersedes an
//! older one, and a finished bitmap whose sequence no longer matches is
//! dropped. resvg cannot be interrupted mid-render, so that drop happens
//! after the raster returns.
//!
//! The target is the on-screen destination ([`image_view_rect`]): the fitted
//! picture times the zoom, at the monitor DPI. Fit is that destination at
//! 100%. Actual size is the intrinsic size at the monitor DPI, which is the
//! destination [`ImageCommand::Actual`] already selects. A bitmap that is
//! already that size is not rendered again.
//!
//! Past [`SvgLimits::max_pixels`] (32 million) the target is scaled down and
//! no larger buffer is requested. Further zoom keeps scaling that capped
//! bitmap.

use crate::geometry::effective_dpi;
use crate::image_preview::ImageFormat;
use crate::image_view::{image_view_rect, ImageFrame, ImageView};
use crate::shell_icon::tick_elapsed_ms;
use crate::svg::{SvgLimits, SVG_MAX_PIXELS};

/// How long the zoom level must stay still before a sharp re-render.
///
/// Long enough that a burst of wheel notches collapses into one raster,
/// short enough that the picture sharpens once the wheel stops. Not a
/// measured Windows frame time.
pub const SVG_ZOOM_SETTLE_MS: u64 = 120;

/// What to do with the SVG bitmap at this zoom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgZoomPlan {
    /// Not an SVG, nothing to draw, or the stored bitmap is already the target.
    Skip,
    /// Rasterise at this size. `width * height` is within the pixel cap.
    Render { width: u32, height: u32 },
    /// The destination is above the cap and the bitmap is already the capped
    /// size. Keep scaling it. Do not allocate a larger buffer.
    KeepScaling { width: u32, height: u32 },
}

/// On-screen destination in physical pixels. `(0, 0)` when it cannot be drawn.
pub fn svg_display_pixels(frame: ImageFrame, view: ImageView) -> (u32, u32) {
    let rect = image_view_rect(frame, view);
    (
        u32::try_from(rect.width()).unwrap_or(0),
        u32::try_from(rect.height()).unwrap_or(0),
    )
}

/// Fit `width`×`height` inside `max_pixels` without changing the aspect ratio.
///
/// The result is never larger than the input. A zero side stays zero. When
/// the input is already under the cap it is returned unchanged, so a caller
/// can compare it to the current bitmap.
pub fn cap_svg_render_size(width: u32, height: u32, max_pixels: u64) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (0, 0);
    }
    let max_pixels = max_pixels.max(1);
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels <= max_pixels {
        return (width, height);
    }
    let scale = (max_pixels as f64 / pixels as f64).sqrt();
    let mut w = ((f64::from(width) * scale).floor() as u32).max(1);
    let mut h = ((f64::from(height) * scale).floor() as u32).max(1);
    // `floor` can leave the product one step over the cap.
    while u64::from(w).saturating_mul(u64::from(h)) > max_pixels {
        if w >= h && w > 1 {
            w -= 1;
        } else if h > 1 {
            h -= 1;
        } else {
            break;
        }
    }
    (w, h)
}

/// Pixel cap used for a sharp SVG raster. Same value as [`SvgLimits::default`].
pub fn svg_sharp_max_pixels() -> u64 {
    let limit = SvgLimits::default().max_pixels;
    debug_assert!(limit <= SVG_MAX_PIXELS);
    limit
}

/// Whether this view should re-rasterise, and at which size.
///
/// `frame.image_w` / `image_h` is the bitmap on screen. `source_*` and the
/// view decide the destination. Non-SVG formats never request a render.
pub fn svg_zoom_plan(
    format: ImageFormat,
    frame: ImageFrame,
    view: ImageView,
    max_pixels: u64,
) -> SvgZoomPlan {
    if format != ImageFormat::Svg {
        return SvgZoomPlan::Skip;
    }
    let max_pixels = max_pixels.max(1);
    let (display_w, display_h) = svg_display_pixels(frame, view);
    if display_w == 0 || display_h == 0 {
        return SvgZoomPlan::Skip;
    }
    let (width, height) = cap_svg_render_size(display_w, display_h, max_pixels);
    if width == 0 || height == 0 {
        return SvgZoomPlan::Skip;
    }
    let over_cap = u64::from(display_w).saturating_mul(u64::from(display_h)) > max_pixels;
    if frame.image_w == width && frame.image_h == height {
        if over_cap {
            SvgZoomPlan::KeepScaling { width, height }
        } else {
            SvgZoomPlan::Skip
        }
    } else {
        SvgZoomPlan::Render { width, height }
    }
}

/// What the UI should do after one zoom-session event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgZoomEffect {
    /// Wait this long, then deliver [`SvgZoomEvent::Tick`].
    ArmTimer {
        delay_ms: u64,
    },
    DisarmTimer,
    /// Background raster. `seq` is the only result that may be shown.
    StartRender {
        seq: u64,
        file_gen: u64,
        width: u32,
        height: u32,
    },
    /// The raster is current and the UI is idle. Replace the previous bitmap.
    SwapBitmap {
        seq: u64,
        file_gen: u64,
        width: u32,
        height: u32,
    },
    /// A raster finished during a pan or a border drag. Hold it until idle.
    HoldBitmap {
        seq: u64,
        file_gen: u64,
        width: u32,
        height: u32,
    },
    /// This result is stale, failed, or not the bitmap we asked for.
    DropBitmap,
    /// Drop any bitmap held back during a drag.
    DiscardHeld,
}

/// One observation of the picture the window is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SvgZoomSnap {
    pub format: ImageFormat,
    pub frame: ImageFrame,
    pub view: ImageView,
    /// Preview generation of the file on screen. A different file is stale.
    pub file_gen: u64,
    pub max_pixels: u64,
}

/// Inputs the window procedure feeds in. The clock is a wrapping millisecond
/// tick, the same wrap as `GetTickCount`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvgZoomEvent {
    /// The preview installed this picture. A matching bitmap does not render.
    Shown {
        now: u32,
        snap: SvgZoomSnap,
    },
    /// Zoom, fit, actual size, or a body/DPI change.
    ViewChanged {
        now: u32,
        snap: SvgZoomSnap,
    },
    PanStarted,
    PanEnded {
        now: u32,
        snap: Option<SvgZoomSnap>,
    },
    SizeDragStarted,
    SizeDragEnded {
        now: u32,
        snap: Option<SvgZoomSnap>,
    },
    Tick {
        now: u32,
        snap: Option<SvgZoomSnap>,
    },
    /// Esc, a new file, or the window closing.
    Cancel,
    /// The preview is no longer an SVG.
    Cleared,
    Finished {
        seq: u64,
        file_gen: u64,
        width: u32,
        height: u32,
        ok: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ViewKey {
    file_gen: u64,
    body_w: i32,
    body_h: i32,
    dpi: u32,
    zoom_milli: u32,
    fitted: bool,
    source_w: u32,
    source_h: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Inflight {
    seq: u64,
    file_gen: u64,
    width: u32,
    height: u32,
}

/// Latest-wins zoom session. Cheap to keep on the UI thread: no pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvgZoomSession {
    active: bool,
    file_gen: u64,
    pending_since: Option<u32>,
    pending_key: Option<ViewKey>,
    seq: u64,
    inflight: Option<Inflight>,
    deferred: Option<Inflight>,
    panning: bool,
    sizing: bool,
}

impl Default for SvgZoomSession {
    fn default() -> Self {
        Self {
            active: false,
            file_gen: 0,
            pending_since: None,
            pending_key: None,
            seq: 0,
            inflight: None,
            deferred: None,
            panning: false,
            sizing: false,
        }
    }
}

impl SvgZoomSession {
    pub fn handle(&mut self, event: SvgZoomEvent) -> Vec<SvgZoomEffect> {
        match event {
            SvgZoomEvent::Cancel | SvgZoomEvent::Cleared => self.cancel(),
            SvgZoomEvent::Shown { now, snap } | SvgZoomEvent::ViewChanged { now, snap } => {
                self.note_view(now, &snap)
            }
            SvgZoomEvent::PanStarted => {
                self.panning = true;
                vec![SvgZoomEffect::DisarmTimer]
            }
            SvgZoomEvent::SizeDragStarted => {
                self.sizing = true;
                vec![SvgZoomEffect::DisarmTimer]
            }
            SvgZoomEvent::PanEnded { now, snap } => {
                self.panning = false;
                self.resume(now, snap.as_ref())
            }
            SvgZoomEvent::SizeDragEnded { now, snap } => {
                self.sizing = false;
                self.resume(now, snap.as_ref())
            }
            SvgZoomEvent::Tick { now, snap } => self.poll(now, snap.as_ref()),
            SvgZoomEvent::Finished {
                seq,
                file_gen,
                width,
                height,
                ok,
            } => self.finish(seq, file_gen, width, height, ok),
        }
    }

    fn cancel(&mut self) -> Vec<SvgZoomEffect> {
        let drop_held = self.invalidate_inflight();
        self.active = false;
        self.file_gen = 0;
        self.pending_since = None;
        self.pending_key = None;
        self.panning = false;
        self.sizing = false;
        let mut effects = drop_held;
        effects.push(SvgZoomEffect::DisarmTimer);
        effects
    }

    fn note_view(&mut self, now: u32, snap: &SvgZoomSnap) -> Vec<SvgZoomEffect> {
        if snap.format != ImageFormat::Svg {
            return self.cancel();
        }
        let mut effects = Vec::new();
        if !self.active || self.file_gen != snap.file_gen {
            effects.extend(self.invalidate_inflight());
            self.active = true;
            self.file_gen = snap.file_gen;
            self.pending_since = None;
            self.pending_key = None;
            // A new file is not mid-drag. The window resets the gesture too.
            self.panning = false;
            self.sizing = false;
        }
        let key = view_key(snap);
        if self.pending_key == Some(key) {
            return effects;
        }
        match svg_zoom_plan(snap.format, snap.frame, snap.view, snap.max_pixels) {
            SvgZoomPlan::Skip => {
                self.pending_since = None;
                self.pending_key = None;
                effects.extend(self.invalidate_inflight());
                effects.push(SvgZoomEffect::DisarmTimer);
            }
            SvgZoomPlan::KeepScaling { width, height } => {
                self.pending_since = None;
                self.pending_key = None;
                if self.inflight.is_some_and(|inf| {
                    inf.file_gen != snap.file_gen || inf.width != width || inf.height != height
                }) {
                    effects.extend(self.invalidate_inflight());
                }
                effects.push(SvgZoomEffect::DisarmTimer);
            }
            SvgZoomPlan::Render { width, height } => {
                let same = self.inflight.is_some_and(|inf| {
                    inf.file_gen == snap.file_gen && inf.width == width && inf.height == height
                });
                if same {
                    self.pending_since = None;
                    self.pending_key = None;
                    effects.push(SvgZoomEffect::DisarmTimer);
                } else {
                    effects.extend(self.invalidate_inflight());
                    self.pending_since = Some(now);
                    self.pending_key = Some(key);
                    effects.push(SvgZoomEffect::ArmTimer {
                        delay_ms: SVG_ZOOM_SETTLE_MS,
                    });
                }
            }
        }
        effects
    }

    fn resume(&mut self, now: u32, snap: Option<&SvgZoomSnap>) -> Vec<SvgZoomEffect> {
        let mut effects = self.take_deferred_swap();
        effects.extend(self.poll(now, snap));
        effects
    }

    fn poll(&mut self, now: u32, snap: Option<&SvgZoomSnap>) -> Vec<SvgZoomEffect> {
        if self.panning || self.sizing || !self.active {
            return Vec::new();
        }
        let Some(since) = self.pending_since else {
            return Vec::new();
        };
        let Some(snap) = snap else {
            return Vec::new();
        };
        if snap.format != ImageFormat::Svg || snap.file_gen != self.file_gen {
            return Vec::new();
        }
        if self.pending_key != Some(view_key(snap)) {
            return self.note_view(now, snap);
        }
        let elapsed = tick_elapsed_ms(since, now);
        if elapsed < SVG_ZOOM_SETTLE_MS {
            return vec![SvgZoomEffect::ArmTimer {
                delay_ms: (SVG_ZOOM_SETTLE_MS - elapsed).max(1),
            }];
        }
        self.pending_since = None;
        self.pending_key = None;
        self.begin(snap)
    }

    fn begin(&mut self, snap: &SvgZoomSnap) -> Vec<SvgZoomEffect> {
        match svg_zoom_plan(snap.format, snap.frame, snap.view, snap.max_pixels) {
            SvgZoomPlan::Render { width, height } => {
                if self.inflight.is_some_and(|inf| {
                    inf.file_gen == snap.file_gen && inf.width == width && inf.height == height
                }) {
                    return vec![SvgZoomEffect::DisarmTimer];
                }
                let mut effects = self.invalidate_inflight();
                self.seq = self.seq.saturating_add(1);
                let seq = self.seq;
                self.inflight = Some(Inflight {
                    seq,
                    file_gen: snap.file_gen,
                    width,
                    height,
                });
                effects.push(SvgZoomEffect::DisarmTimer);
                effects.push(SvgZoomEffect::StartRender {
                    seq,
                    file_gen: snap.file_gen,
                    width,
                    height,
                });
                effects
            }
            SvgZoomPlan::Skip => {
                let mut effects = self.invalidate_inflight();
                effects.push(SvgZoomEffect::DisarmTimer);
                effects
            }
            SvgZoomPlan::KeepScaling { width, height } => {
                let mut effects = Vec::new();
                if self.inflight.is_some_and(|inf| {
                    inf.file_gen != snap.file_gen || inf.width != width || inf.height != height
                }) {
                    effects.extend(self.invalidate_inflight());
                }
                effects.push(SvgZoomEffect::DisarmTimer);
                effects
            }
        }
    }

    fn take_deferred_swap(&mut self) -> Vec<SvgZoomEffect> {
        let Some(deferred) = self.deferred.take() else {
            return Vec::new();
        };
        let current = self.inflight.is_some_and(|inf| inf.seq == deferred.seq);
        if !current {
            return vec![SvgZoomEffect::DiscardHeld];
        }
        self.inflight = None;
        vec![SvgZoomEffect::SwapBitmap {
            seq: deferred.seq,
            file_gen: deferred.file_gen,
            width: deferred.width,
            height: deferred.height,
        }]
    }

    fn finish(
        &mut self,
        seq: u64,
        file_gen: u64,
        width: u32,
        height: u32,
        ok: bool,
    ) -> Vec<SvgZoomEffect> {
        let Some(inf) = self.inflight else {
            return vec![SvgZoomEffect::DropBitmap];
        };
        if inf.seq != seq || inf.file_gen != file_gen {
            return vec![SvgZoomEffect::DropBitmap];
        }
        if !ok || inf.width != width || inf.height != height || width == 0 || height == 0 {
            self.inflight = None;
            return vec![SvgZoomEffect::DropBitmap];
        }
        if self.panning || self.sizing {
            self.deferred = Some(Inflight {
                seq,
                file_gen,
                width,
                height,
            });
            return vec![SvgZoomEffect::HoldBitmap {
                seq,
                file_gen,
                width,
                height,
            }];
        }
        self.inflight = None;
        self.deferred = None;
        vec![SvgZoomEffect::SwapBitmap {
            seq,
            file_gen,
            width,
            height,
        }]
    }

    fn invalidate_inflight(&mut self) -> Vec<SvgZoomEffect> {
        // Take both. `||` would skip the deferred bitmap when a render is
        // still marked in flight, and a drag-end could swap that stale raster.
        let had_inflight = self.inflight.take().is_some();
        let had_deferred = self.deferred.take().is_some();
        if had_inflight || had_deferred {
            vec![SvgZoomEffect::DiscardHeld]
        } else {
            Vec::new()
        }
    }
}

fn view_key(snap: &SvgZoomSnap) -> ViewKey {
    ViewKey {
        file_gen: snap.file_gen,
        body_w: snap.frame.body.width(),
        body_h: snap.frame.body.height(),
        dpi: effective_dpi(snap.frame.dpi),
        zoom_milli: snap.view.zoom_milli(),
        fitted: snap.view.is_fitted(),
        source_w: snap.frame.source_w,
        source_h: snap.frame.source_h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::PxRect;
    use crate::image_view::{apply_image_command, ImageCommand};

    fn body(w: i32, h: i32) -> PxRect {
        PxRect {
            left: 0,
            top: 0,
            right: w,
            bottom: h,
        }
    }

    fn frame(
        body_w: i32,
        body_h: i32,
        source_w: u32,
        source_h: u32,
        image_w: u32,
        image_h: u32,
        dpi: u32,
    ) -> ImageFrame {
        ImageFrame {
            body: body(body_w, body_h),
            image_w,
            image_h,
            source_w,
            source_h,
            dpi,
        }
    }

    fn snap(frame: ImageFrame, view: ImageView, file_gen: u64, max_pixels: u64) -> SvgZoomSnap {
        SvgZoomSnap {
            format: ImageFormat::Svg,
            frame,
            view,
            file_gen,
            max_pixels,
        }
    }

    fn zoom_in(view: ImageView, frame: ImageFrame, steps: u32) -> ImageView {
        apply_image_command(view, frame, ImageCommand::ZoomIn { steps }, None)
    }

    fn renders(effects: &[SvgZoomEffect]) -> Vec<(u64, u32, u32)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                SvgZoomEffect::StartRender {
                    seq, width, height, ..
                } => Some((*seq, *width, *height)),
                _ => None,
            })
            .collect()
    }

    fn swapped(effects: &[SvgZoomEffect]) -> bool {
        effects
            .iter()
            .any(|effect| matches!(effect, SvgZoomEffect::SwapBitmap { .. }))
    }

    fn held(effects: &[SvgZoomEffect]) -> bool {
        effects
            .iter()
            .any(|effect| matches!(effect, SvgZoomEffect::HoldBitmap { .. }))
    }

    #[test]
    fn settle_is_inside_the_requested_window() {
        assert!((80..=150).contains(&SVG_ZOOM_SETTLE_MS));
        assert!(svg_sharp_max_pixels() <= SVG_MAX_PIXELS);
        assert!(svg_sharp_max_pixels() <= crate::image_preview::ImageLimits::default().max_pixels);
    }

    #[test]
    fn target_pixels_follow_zoom_and_dpi() {
        let max_pixels = svg_sharp_max_pixels();

        // Fit at 96 DPI. The bitmap is the body, so nothing is rendered.
        let fit_frame = frame(800, 600, 800, 600, 800, 600, 96);
        assert_eq!(svg_display_pixels(fit_frame, ImageView::fit()), (800, 600));
        assert_eq!(
            svg_zoom_plan(ImageFormat::Svg, fit_frame, ImageView::fit(), max_pixels),
            SvgZoomPlan::Skip
        );

        // The stored bitmap is the old fit. One 5/4 step draws 1000×750.
        let stepped = zoom_in(ImageView::fit(), fit_frame, 1);
        assert_eq!(stepped.zoom_milli(), 1_250);
        assert_eq!(svg_display_pixels(fit_frame, stepped), (1_000, 750));
        assert_eq!(
            svg_zoom_plan(ImageFormat::Svg, fit_frame, stepped, max_pixels),
            SvgZoomPlan::Render {
                width: 1_000,
                height: 750
            }
        );

        // Two steps: 1250 * 5/4 = 1562. 800*1562/1000 = 1249, 600*1562/1000 = 937.
        let twice = zoom_in(ImageView::fit(), fit_frame, 2);
        assert_eq!(twice.zoom_milli(), 1_562);
        assert_eq!(svg_display_pixels(fit_frame, twice), (1_249, 937));
        assert_eq!(
            svg_zoom_plan(ImageFormat::Svg, fit_frame, twice, max_pixels),
            SvgZoomPlan::Render {
                width: 1_249,
                height: 937
            }
        );

        // 144 DPI scales a 96×48 CSS image to 144×72, and fit does not upscale
        // past that. A 96×48 bitmap is short of the destination.
        let dpi_frame = frame(400, 400, 96, 48, 96, 48, 144);
        assert_eq!(svg_display_pixels(dpi_frame, ImageView::fit()), (144, 72));
        assert_eq!(
            svg_zoom_plan(ImageFormat::Svg, dpi_frame, ImageView::fit(), max_pixels),
            SvgZoomPlan::Render {
                width: 144,
                height: 72
            }
        );

        // 192 DPI, actual size. Source 400 CSS px in a 200 px body.
        // Desired = 800. The view's milli is 4000, so the destination is 800.
        let actual_frame = frame(200, 200, 400, 400, 200, 200, 192);
        let actual =
            apply_image_command(ImageView::fit(), actual_frame, ImageCommand::Actual, None);
        assert!(!actual.is_fitted());
        assert_eq!(svg_display_pixels(actual_frame, actual), (800, 800));
        assert_eq!(
            svg_zoom_plan(ImageFormat::Svg, actual_frame, actual, max_pixels),
            SvgZoomPlan::Render {
                width: 800,
                height: 800
            }
        );

        // Already at the destination: do not re-render.
        let matched = frame(800, 600, 800, 600, 1_000, 750, 96);
        assert_eq!(
            svg_zoom_plan(ImageFormat::Svg, matched, stepped, max_pixels),
            SvgZoomPlan::Skip
        );
    }

    #[test]
    fn above_the_cap_keeps_scaling_and_does_not_ask_for_a_larger_buffer() {
        let requested = 8_000u32;
        let (capped_w, capped_h) = cap_svg_render_size(requested, requested, SVG_MAX_PIXELS);
        let capped_pixels = u64::from(capped_w).saturating_mul(u64::from(capped_h));
        assert!(capped_pixels <= SVG_MAX_PIXELS, "{capped_pixels}");
        assert!(capped_pixels > SVG_MAX_PIXELS / 2, "{capped_pixels}");
        assert!(capped_w < requested && capped_h < requested);
        assert_eq!(
            cap_svg_render_size(10, 10, SVG_MAX_PIXELS),
            (10, 10),
            "under the cap the size is unchanged"
        );

        // 800% of a 4_000 px fit is 32_000 px on a side, far past 32 MP.
        let huge = frame(4_000, 4_000, 4_000, 4_000, 100, 100, 96);
        let view = zoom_in(ImageView::fit(), huge, 64);
        let (display_w, display_h) = svg_display_pixels(huge, view);
        assert!(u64::from(display_w).saturating_mul(u64::from(display_h)) > SVG_MAX_PIXELS);
        let plan = svg_zoom_plan(ImageFormat::Svg, huge, view, SVG_MAX_PIXELS);
        let SvgZoomPlan::Render { width, height } = plan else {
            panic!("a small bitmap still renders once at the cap, got {plan:?}");
        };
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        assert!(pixels <= SVG_MAX_PIXELS, "{pixels}");
        assert!(width < display_w && height < display_h);
        assert_ne!((width, height), (display_w, display_h));

        let already = frame(4_000, 4_000, 4_000, 4_000, width, height, 96);
        assert_eq!(
            svg_zoom_plan(ImageFormat::Svg, already, view, SVG_MAX_PIXELS),
            SvgZoomPlan::KeepScaling { width, height }
        );
    }

    #[test]
    fn non_svg_formats_do_not_request_a_render() {
        let frame = frame(800, 600, 800, 600, 100, 100, 96);
        let view = zoom_in(ImageView::fit(), frame, 1);
        for format in [
            ImageFormat::Png,
            ImageFormat::Jpeg,
            ImageFormat::Bmp,
            ImageFormat::Gif,
            ImageFormat::Ico,
            ImageFormat::WebP,
            ImageFormat::Pdf,
        ] {
            assert_eq!(
                svg_zoom_plan(format, frame, view, SVG_MAX_PIXELS),
                SvgZoomPlan::Skip,
                "{format:?}"
            );
        }
        let mut session = SvgZoomSession::default();
        let mut png = snap(frame, view, 1, SVG_MAX_PIXELS);
        png.format = ImageFormat::Png;
        let effects = session.handle(SvgZoomEvent::Shown { now: 0, snap: png });
        assert!(renders(&effects).is_empty());
        let effects = session.handle(SvgZoomEvent::Tick {
            now: 1_000,
            snap: Some(png),
        });
        assert!(renders(&effects).is_empty());
        assert!(!session.active);
    }

    #[test]
    fn too_soon_tick_does_not_render_and_a_settled_zoom_does() {
        let base = frame(800, 600, 800, 600, 800, 600, 96);
        let view = zoom_in(ImageView::fit(), base, 1);
        let picture = snap(base, view, 7, SVG_MAX_PIXELS);
        let mut session = SvgZoomSession::default();
        let armed = session.handle(SvgZoomEvent::ViewChanged {
            now: 1_000,
            snap: picture,
        });
        assert!(
            renders(&armed).is_empty(),
            "the zoom itself must not start a raster"
        );
        assert!(armed.contains(&SvgZoomEffect::ArmTimer {
            delay_ms: SVG_ZOOM_SETTLE_MS
        }));

        let early = session.handle(SvgZoomEvent::Tick {
            now: 1_050,
            snap: Some(picture),
        });
        assert!(
            renders(&early).is_empty(),
            "50 ms is inside the settle window"
        );
        assert_eq!(
            early,
            vec![SvgZoomEffect::ArmTimer {
                delay_ms: SVG_ZOOM_SETTLE_MS - 50
            }]
        );

        let settled = session.handle(SvgZoomEvent::Tick {
            now: 1_000 + u32::try_from(SVG_ZOOM_SETTLE_MS).unwrap(),
            snap: Some(picture),
        });
        assert_eq!(renders(&settled), vec![(1, 1_000, 750)]);
    }

    #[test]
    fn newer_zoom_supersedes_and_a_stale_finish_is_dropped() {
        let base = frame(800, 600, 800, 600, 800, 600, 96);
        let first = zoom_in(ImageView::fit(), base, 1);
        let second = zoom_in(ImageView::fit(), base, 2);
        let mut session = SvgZoomSession::default();
        let first_snap = snap(base, first, 3, SVG_MAX_PIXELS);
        session.handle(SvgZoomEvent::ViewChanged {
            now: 0,
            snap: first_snap,
        });
        let early = session.handle(SvgZoomEvent::Tick {
            now: 40,
            snap: Some(first_snap),
        });
        assert!(renders(&early).is_empty());

        let second_snap = snap(base, second, 3, SVG_MAX_PIXELS);
        // Let the first target start, then move the zoom before it lands.
        let first_started = session.handle(SvgZoomEvent::Tick {
            now: SVG_ZOOM_SETTLE_MS as u32,
            snap: Some(first_snap),
        });
        let (first_seq, _, _) = renders(&first_started)[0];

        let replaced = session.handle(SvgZoomEvent::ViewChanged {
            now: 40 + SVG_ZOOM_SETTLE_MS as u32,
            snap: second_snap,
        });
        assert!(
            renders(&replaced).is_empty(),
            "a newer zoom restarts the settle clock"
        );
        assert!(replaced.contains(&SvgZoomEffect::DiscardHeld));
        assert!(session.inflight.is_none());

        let stale = session.handle(SvgZoomEvent::Finished {
            seq: first_seq,
            file_gen: 3,
            width: 1_000,
            height: 750,
            ok: true,
        });
        assert_eq!(stale, vec![SvgZoomEffect::DropBitmap]);
        assert!(!swapped(&stale));

        // The first target's original deadline has elapsed. The clock belongs
        // to the newer zoom, which has not settled yet.
        let still_early = session.handle(SvgZoomEvent::Tick {
            now: 40 + SVG_ZOOM_SETTLE_MS as u32 + 40,
            snap: Some(second_snap),
        });
        assert!(renders(&still_early).is_empty());

        let started = session.handle(SvgZoomEvent::Tick {
            now: 40 + 2 * SVG_ZOOM_SETTLE_MS as u32,
            snap: Some(second_snap),
        });
        let (seq, width, height) = renders(&started)[0];
        assert_eq!((width, height), (1_249, 937));
        assert_ne!(seq, first_seq);

        let fresh = session.handle(SvgZoomEvent::Finished {
            seq,
            file_gen: 3,
            width,
            height,
            ok: true,
        });
        assert!(swapped(&fresh));
        assert!(!held(&fresh));
    }

    #[test]
    fn a_new_file_and_cancel_drop_an_in_flight_raster() {
        let base = frame(400, 300, 400, 300, 400, 300, 96);
        let view = zoom_in(ImageView::fit(), base, 1);
        let mut session = SvgZoomSession::default();
        let picture = snap(base, view, 1, SVG_MAX_PIXELS);
        session.handle(SvgZoomEvent::ViewChanged {
            now: 0,
            snap: picture,
        });
        let started = session.handle(SvgZoomEvent::Tick {
            now: SVG_ZOOM_SETTLE_MS as u32,
            snap: Some(picture),
        });
        let (seq, width, height) = renders(&started)[0];

        let other = snap(base, view, 2, SVG_MAX_PIXELS);
        let switched = session.handle(SvgZoomEvent::Shown {
            now: 5,
            snap: other,
        });
        assert!(switched.contains(&SvgZoomEffect::DiscardHeld));
        let late = session.handle(SvgZoomEvent::Finished {
            seq,
            file_gen: 1,
            width,
            height,
            ok: true,
        });
        assert_eq!(late, vec![SvgZoomEffect::DropBitmap]);

        session.handle(SvgZoomEvent::ViewChanged {
            now: 10,
            snap: other,
        });
        let again = session.handle(SvgZoomEvent::Tick {
            now: 10 + SVG_ZOOM_SETTLE_MS as u32,
            snap: Some(other),
        });
        let (seq, _, _) = renders(&again)[0];
        let cancelled = session.handle(SvgZoomEvent::Cancel);
        assert!(cancelled.contains(&SvgZoomEffect::DisarmTimer));
        assert!(!session.active);
        let after_esc = session.handle(SvgZoomEvent::Finished {
            seq,
            file_gen: 2,
            width,
            height,
            ok: true,
        });
        assert_eq!(after_esc, vec![SvgZoomEffect::DropBitmap]);
        assert!(!swapped(&after_esc));
    }

    #[test]
    fn resize_that_changes_the_fit_supersedes_the_old_target() {
        // The source is larger than either body, so the fit destination
        // follows the body. A wider window is a different raster.
        let before = frame(400, 300, 2_000, 1_500, 400, 300, 96);
        let after = frame(800, 600, 2_000, 1_500, 400, 300, 96);
        let mut session = SvgZoomSession::default();
        let first = snap(before, ImageView::fit(), 4, SVG_MAX_PIXELS);
        // Bitmap matches the 400×300 fit, so the first view does not render.
        // Zoom, let it start, then change the body.
        let zoomed = zoom_in(ImageView::fit(), before, 1);
        let zoomed_snap = snap(before, zoomed, 4, SVG_MAX_PIXELS);
        session.handle(SvgZoomEvent::Shown {
            now: 0,
            snap: first,
        });
        session.handle(SvgZoomEvent::ViewChanged {
            now: 0,
            snap: zoomed_snap,
        });
        let started = session.handle(SvgZoomEvent::Tick {
            now: SVG_ZOOM_SETTLE_MS as u32,
            snap: Some(zoomed_snap),
        });
        let (seq, _, _) = renders(&started)[0];

        let resized = snap(after, zoomed, 4, SVG_MAX_PIXELS);
        let moved = session.handle(SvgZoomEvent::ViewChanged {
            now: 500,
            snap: resized,
        });
        assert!(renders(&moved).is_empty());
        assert!(moved.contains(&SvgZoomEffect::DiscardHeld));
        let stale = session.handle(SvgZoomEvent::Finished {
            seq,
            file_gen: 4,
            width: 500,
            height: 375,
            ok: true,
        });
        assert_eq!(stale, vec![SvgZoomEffect::DropBitmap]);

        let next = session.handle(SvgZoomEvent::Tick {
            now: 500 + SVG_ZOOM_SETTLE_MS as u32,
            snap: Some(resized),
        });
        let (_seq, width, height) = renders(&next)[0];
        let (expect_w, expect_h) = svg_display_pixels(after, zoomed);
        assert_eq!((width, height), (expect_w, expect_h));
        assert_ne!((width, height), svg_display_pixels(before, zoomed));
    }

    #[test]
    fn pan_and_border_drag_hold_the_swap_until_idle() {
        let base = frame(800, 600, 800, 600, 800, 600, 96);
        let view = zoom_in(ImageView::fit(), base, 1);
        let picture = snap(base, view, 9, SVG_MAX_PIXELS);
        let mut session = SvgZoomSession::default();
        session.handle(SvgZoomEvent::ViewChanged {
            now: 0,
            snap: picture,
        });
        let started = session.handle(SvgZoomEvent::Tick {
            now: SVG_ZOOM_SETTLE_MS as u32,
            snap: Some(picture),
        });
        let (seq, width, height) = renders(&started)[0];

        assert!(session
            .handle(SvgZoomEvent::PanStarted)
            .contains(&SvgZoomEffect::DisarmTimer));
        let during = session.handle(SvgZoomEvent::Finished {
            seq,
            file_gen: 9,
            width,
            height,
            ok: true,
        });
        assert!(held(&during));
        assert!(!swapped(&during));
        let tick_while_dragging = session.handle(SvgZoomEvent::Tick {
            now: 10_000,
            snap: Some(picture),
        });
        assert!(renders(&tick_while_dragging).is_empty());
        assert!(!swapped(&tick_while_dragging));

        let ended = session.handle(SvgZoomEvent::PanEnded {
            now: 10_000,
            snap: Some(picture),
        });
        assert!(swapped(&ended));
        assert!(!held(&ended));

        // A border drag does the same for a later raster.
        session.handle(SvgZoomEvent::ViewChanged {
            now: 11_000,
            snap: snap(base, zoom_in(ImageView::fit(), base, 2), 9, SVG_MAX_PIXELS),
        });
        let picture = snap(base, zoom_in(ImageView::fit(), base, 2), 9, SVG_MAX_PIXELS);
        let started = session.handle(SvgZoomEvent::Tick {
            now: 11_000 + SVG_ZOOM_SETTLE_MS as u32,
            snap: Some(picture),
        });
        let (seq, width, height) = renders(&started)[0];
        session.handle(SvgZoomEvent::SizeDragStarted);
        let during = session.handle(SvgZoomEvent::Finished {
            seq,
            file_gen: 9,
            width,
            height,
            ok: true,
        });
        assert!(held(&during) && !swapped(&during));
        let ended = session.handle(SvgZoomEvent::SizeDragEnded {
            now: 12_000,
            snap: Some(picture),
        });
        assert!(swapped(&ended));
    }

    #[test]
    fn a_failed_raster_is_dropped_and_the_same_view_is_not_swapped() {
        let base = frame(200, 200, 200, 200, 100, 100, 96);
        let picture = snap(base, ImageView::fit(), 1, SVG_MAX_PIXELS);
        let mut session = SvgZoomSession::default();
        session.handle(SvgZoomEvent::Shown {
            now: 0,
            snap: picture,
        });
        let started = session.handle(SvgZoomEvent::Tick {
            now: SVG_ZOOM_SETTLE_MS as u32,
            snap: Some(picture),
        });
        let (seq, _, _) = renders(&started)[0];
        let failed = session.handle(SvgZoomEvent::Finished {
            seq,
            file_gen: 1,
            width: 0,
            height: 0,
            ok: false,
        });
        assert_eq!(failed, vec![SvgZoomEffect::DropBitmap]);
        assert!(session.inflight.is_none());
    }

    #[test]
    fn same_view_does_not_restart_the_settle_clock() {
        let base = frame(800, 600, 800, 600, 800, 600, 96);
        let view = zoom_in(ImageView::fit(), base, 1);
        let picture = snap(base, view, 1, SVG_MAX_PIXELS);
        let mut session = SvgZoomSession::default();
        session.handle(SvgZoomEvent::ViewChanged {
            now: 0,
            snap: picture,
        });
        let again = session.handle(SvgZoomEvent::ViewChanged {
            now: 1_000,
            snap: picture,
        });
        assert!(
            again.is_empty(),
            "a repeated size must not push the deadline out"
        );
        let started = session.handle(SvgZoomEvent::Tick {
            now: SVG_ZOOM_SETTLE_MS as u32,
            snap: Some(picture),
        });
        assert_eq!(renders(&started).len(), 1);
    }

    #[test]
    fn tick_wraps_the_same_way_as_get_tick_count() {
        let base = frame(800, 600, 800, 600, 800, 600, 96);
        let picture = snap(base, zoom_in(ImageView::fit(), base, 1), 1, SVG_MAX_PIXELS);
        let mut session = SvgZoomSession::default();
        let now = u32::MAX - 30;
        session.handle(SvgZoomEvent::ViewChanged { now, snap: picture });
        let started = session.handle(SvgZoomEvent::Tick {
            now: now.wrapping_add(SVG_ZOOM_SETTLE_MS as u32),
            snap: Some(picture),
        });
        assert_eq!(renders(&started).len(), 1);
    }
}
