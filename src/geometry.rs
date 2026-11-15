/// Logical preview width at 96 DPI. Physical pixels are `MulDiv(width, dpi, 96)`.
pub const LOGICAL_PREVIEW_WIDTH: i32 = 760;
/// Logical preview height at 96 DPI.
pub const LOGICAL_PREVIEW_HEIGHT: i32 = 520;
/// Logical font height in pixels at 96 DPI. Passed to `CreateFontW` negated.
pub const LOGICAL_FONT_HEIGHT: i32 = 18;
/// DPI that the logical sizes above are authored at.
pub const BASE_DPI: i32 = 96;

/// Inclusive-exclusive rectangle in physical pixels. Origin may be negative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PxRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl PxRect {
    pub fn width(self) -> i32 {
        self.right.saturating_sub(self.left).max(0)
    }

    pub fn height(self) -> i32 {
        self.bottom.saturating_sub(self.top).max(0)
    }

    fn center(self) -> (i32, i32) {
        (
            self.left.saturating_add(self.width() / 2),
            self.top.saturating_add(self.height() / 2),
        )
    }

    fn has_area(self) -> bool {
        self.right > self.left && self.bottom > self.top
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PxSize {
    pub width: i32,
    pub height: i32,
}

/// Top-left and size of the preview window, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Treat a missing DPI reading as 96 so a failed query still matches the
/// authored logical sizes.
pub fn effective_dpi(dpi: u32) -> u32 {
    if dpi == 0 {
        96
    } else {
        dpi
    }
}

/// Win32 `MulDiv(number, numerator, denominator)`, including the half-away-from-zero
/// rounding and the `-1` overflow / zero-denominator results.
pub fn muldiv(number: i32, numerator: i32, denominator: i32) -> i32 {
    if denominator == 0 {
        return -1;
    }
    let mut number = i64::from(number);
    let numerator = i64::from(numerator);
    let mut denominator = i64::from(denominator);
    if denominator < 0 {
        number = -number;
        denominator = -denominator;
    }
    let product = number * numerator;
    let result = if product >= 0 {
        (product + denominator / 2) / denominator
    } else {
        (product - denominator / 2) / denominator
    };
    if result > i64::from(i32::MAX) || result < -i64::from(i32::MAX) {
        -1
    } else {
        i32::try_from(result).unwrap_or(-1)
    }
}

/// `MulDiv(value, dpi, 96)`.
pub fn scale_for_dpi(value: i32, dpi: u32) -> i32 {
    muldiv(value, i32::try_from(dpi).unwrap_or(i32::MAX), BASE_DPI)
}

pub fn scaled_preview_size(dpi: u32) -> PxSize {
    let dpi = effective_dpi(dpi);
    PxSize {
        width: scale_for_dpi(LOGICAL_PREVIEW_WIDTH, dpi),
        height: scale_for_dpi(LOGICAL_PREVIEW_HEIGHT, dpi),
    }
}

pub fn scaled_font_height(dpi: u32) -> i32 {
    scale_for_dpi(LOGICAL_FONT_HEIGHT, effective_dpi(dpi))
}

/// Centre a `width` by `height` window on `anchor` (or on `work` when the
/// anchor is missing or empty), then clamp every edge into `work`.
///
/// When the window is larger than the work area it is shrunk to fit, so the
/// result stays inside even on a small monitor.
pub fn place_preview(anchor: Option<PxRect>, work: PxRect, width: i32, height: i32) -> Placement {
    let width = fit_span(width, work.width());
    let height = fit_span(height, work.height());
    let (cx, cy) = match anchor {
        Some(rect) if rect.has_area() => rect.center(),
        _ => work.center(),
    };
    Placement {
        x: clamp_origin(cx.saturating_sub(width / 2), width, work.left, work.right),
        y: clamp_origin(cy.saturating_sub(height / 2), height, work.top, work.bottom),
        width,
        height,
    }
}

/// Scale the logical preview to `dpi`, then [`place_preview`].
pub fn preview_placement(anchor: Option<PxRect>, work: PxRect, dpi: u32) -> Placement {
    let size = scaled_preview_size(dpi);
    place_preview(anchor, work, size.width, size.height)
}

fn fit_span(size: i32, span: i32) -> i32 {
    if size <= 0 || span <= 0 {
        0
    } else {
        size.min(span)
    }
}

fn clamp_origin(origin: i32, size: i32, start: i32, end: i32) -> i32 {
    let mut origin = origin;
    if origin < start {
        origin = start;
    }
    if origin.saturating_add(size) > end {
        origin = end.saturating_sub(size);
    }
    if origin < start {
        origin = start;
    }
    origin
}

/// Border the user is dragging. The discriminants match Win32 `WMSZ_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeGrip {
    /// `WMSZ_LEFT` (1). Width changes; height stays.
    Left,
    /// `WMSZ_RIGHT` (2). Width changes; height stays.
    Right,
    /// `WMSZ_TOP` (3). Height changes; width stays.
    Top,
    /// `WMSZ_TOPLEFT` (4). Both axes change.
    TopLeft,
    /// `WMSZ_TOPRIGHT` (5). Both axes change.
    TopRight,
    /// `WMSZ_BOTTOM` (6). Height changes; width stays.
    Bottom,
    /// `WMSZ_BOTTOMLEFT` (7). Both axes change.
    BottomLeft,
    /// `WMSZ_BOTTOMRIGHT` (8). Both axes change.
    BottomRight,
}

#[derive(Clone, Copy)]
enum DragSide {
    /// Left or top.
    Low,
    /// Right or bottom.
    High,
}

impl SizeGrip {
    /// Map Win32 `WM_SIZING` `wParam`. Unknown values are `None`.
    ///
    /// `WMSZ_LEFT` is 1 through `WMSZ_BOTTOMRIGHT` is 8.
    pub const fn from_wmsz(edge: u32) -> Option<Self> {
        match edge {
            1 => Some(Self::Left),
            2 => Some(Self::Right),
            3 => Some(Self::Top),
            4 => Some(Self::TopLeft),
            5 => Some(Self::TopRight),
            6 => Some(Self::Bottom),
            7 => Some(Self::BottomLeft),
            8 => Some(Self::BottomRight),
            _ => None,
        }
    }

    const fn horizontal(self) -> Option<DragSide> {
        match self {
            Self::Left | Self::TopLeft | Self::BottomLeft => Some(DragSide::Low),
            Self::Right | Self::TopRight | Self::BottomRight => Some(DragSide::High),
            Self::Top | Self::Bottom => None,
        }
    }

    const fn vertical(self) -> Option<DragSide> {
        match self {
            Self::Top | Self::TopLeft | Self::TopRight => Some(DragSide::Low),
            Self::Bottom | Self::BottomLeft | Self::BottomRight => Some(DragSide::High),
            Self::Left | Self::Right => None,
        }
    }
}

/// Resize `proposed` around the centre of `anchor`.
///
/// `anchor` is the outer rect at drag start. Only the edges named by `grip`
/// are read from `proposed`. Each of those edges is mirrored through the
/// anchor centre (`left + right` and `top + bottom`, so an odd size is not
/// rounded). A one-pixel move of an edge changes that axis by two pixels, and
/// repeating it does not walk the centre. A left or right drag leaves the
/// top and bottom at `anchor`. A top or bottom drag leaves the left and
/// right at `anchor`. A corner mirrors each axis on its own: opposite sides
/// move by the same amount, and the horizontal and vertical amounts follow
/// the pointer.
///
/// Clamp rule, per dragged axis, after that reflection:
/// - A span below `min` grows to `min`. The centre stays put when `min` has
///   the same parity as the anchor sum. When the parity differs, the centre
///   moves by half a pixel toward the low edge and stays there.
/// - If the rect lies inside `work`, it is kept.
/// - If a side would cross `work`, the span shrinks to the largest size that
///   is still at least `min` and still centred inside `work`.
/// - If `min` cannot stay centred inside `work`, the window shifts by the
///   smallest amount that puts it inside. Symmetry is broken only then. If
///   `work` itself is shorter than `min` on that axis, the result uses the
///   work-area span.
///
/// An undragged axis is copied from `anchor` and is not clamped, even when
/// that side already lies outside `work`. A degenerate `anchor` (no positive
/// area) is returned as `proposed`. A degenerate `work` axis skips the
/// work-area clamp on that axis. All inputs are physical pixels. DPI scaling
/// belongs to the caller.
pub fn center_anchored_size(
    anchor: PxRect,
    proposed: PxRect,
    grip: SizeGrip,
    min: PxSize,
    work: PxRect,
) -> PxRect {
    if anchor.right <= anchor.left || anchor.bottom <= anchor.top {
        return proposed;
    }
    let (left, right) = size_axis(
        anchor.left,
        anchor.right,
        grip.horizontal(),
        match grip.horizontal() {
            Some(DragSide::Low) => proposed.left,
            Some(DragSide::High) => proposed.right,
            None => 0,
        },
        min.width,
        work.left,
        work.right,
    );
    let (top, bottom) = size_axis(
        anchor.top,
        anchor.bottom,
        grip.vertical(),
        match grip.vertical() {
            Some(DragSide::Low) => proposed.top,
            Some(DragSide::High) => proposed.bottom,
            None => 0,
        },
        min.height,
        work.top,
        work.bottom,
    );
    PxRect {
        left,
        top,
        right,
        bottom,
    }
}

fn size_axis(
    anchor_lo: i32,
    anchor_hi: i32,
    dragged: Option<DragSide>,
    proposed_edge: i32,
    min_span: i32,
    work_lo: i32,
    work_hi: i32,
) -> (i32, i32) {
    let side = match dragged {
        Some(side) => side,
        None => return (anchor_lo, anchor_hi),
    };
    let sum = i64::from(anchor_lo) + i64::from(anchor_hi);
    let edge = i64::from(proposed_edge);
    let mut span = match side {
        DragSide::Low => sum - 2 * edge,
        DragSide::High => 2 * edge - sum,
    };
    let min_span = i64::from(if min_span > 0 { min_span } else { 0 });
    if span < min_span {
        span = min_span;
    }
    let work_lo = i64::from(work_lo);
    let work_hi = i64::from(work_hi);
    if work_hi > work_lo {
        let work_span = work_hi - work_lo;
        // Largest span whose centred edges both lie inside the work area.
        // Both terms have the anchor sum's parity, so the centre still lands
        // on a whole pixel when the span is clamped down to this.
        let max_sym = (sum - 2 * work_lo).min(2 * work_hi - sum);
        if max_sym < min_span {
            let fitted = if min_span < work_span {
                min_span
            } else {
                work_span
            };
            return place_span(sum, fitted, work_lo, work_hi, true);
        }
        if span > max_sym {
            span = max_sym;
        }
    }
    place_span(sum, span, work_lo, work_hi, false)
}

/// `span` centred on `sum / 2`. When `clamp_work` is set, shift into
/// `[work_lo, work_hi]` by the smallest amount, then pin if `span` cannot fit.
fn place_span(sum: i64, span: i64, work_lo: i64, work_hi: i64, clamp_work: bool) -> (i32, i32) {
    let mut lo = (sum - span).div_euclid(2);
    let mut hi = lo + span;
    if clamp_work && work_hi > work_lo {
        if lo < work_lo {
            let shift = work_lo - lo;
            lo += shift;
            hi += shift;
        }
        if hi > work_hi {
            let shift = hi - work_hi;
            lo -= shift;
            hi -= shift;
        }
        if lo < work_lo {
            lo = work_lo;
            hi = work_hi;
        }
    }
    (clamp_i32(lo), clamp_i32(hi))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn work_1920x1080() -> PxRect {
        PxRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        }
    }

    /// 420×380 window flush with the bottom-right corner of a 1920×1080 work area.
    fn bottom_right_anchor() -> PxRect {
        PxRect {
            left: 1500,
            top: 700,
            right: 1920,
            bottom: 1080,
        }
    }

    fn assert_inside(work: PxRect, placed: Placement) {
        assert!(placed.x >= work.left, "{placed:?} vs {work:?}");
        assert!(placed.y >= work.top, "{placed:?} vs {work:?}");
        assert!(
            placed.x.saturating_add(placed.width) <= work.right,
            "{placed:?} vs {work:?}"
        );
        assert!(
            placed.y.saturating_add(placed.height) <= work.bottom,
            "{placed:?} vs {work:?}"
        );
    }

    #[test]
    fn clamps_window_anchored_at_bottom_right_of_1920x1080() {
        let work = work_1920x1080();
        let placed = place_preview(
            Some(bottom_right_anchor()),
            work,
            LOGICAL_PREVIEW_WIDTH,
            LOGICAL_PREVIEW_HEIGHT,
        );
        // Unclamped centre of that anchor is (1330, 630), which hangs off
        // the right and bottom edges (2090×1150).
        assert_eq!(
            placed,
            Placement {
                x: 1160,
                y: 560,
                width: 760,
                height: 520,
            }
        );
        assert_inside(work, placed);
    }

    #[test]
    fn one_pixel_cursor_anchor_centres_on_that_monitor() {
        // Desktop placement uses the cursor as a 1×1 anchor on the monitor
        // that contains it, then the same clamp as Explorer.
        let work = PxRect {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 1080,
        };
        let cursor = PxRect {
            left: 2500,
            top: 400,
            right: 2501,
            bottom: 401,
        };
        let placed = place_preview(Some(cursor), work, 760, 520);
        assert_eq!(
            placed,
            Placement {
                x: 2120,
                y: 140,
                width: 760,
                height: 520,
            }
        );
        assert_inside(work, placed);
    }

    #[test]
    fn centres_in_work_area_when_unanchored() {
        let work = work_1920x1080();
        let placed = place_preview(None, work, 760, 520);
        assert_eq!(
            placed,
            Placement {
                x: 580,
                y: 280,
                width: 760,
                height: 520,
            }
        );
        // A taskbar that shrinks rcWork must be honoured. This is not SM_CYSCREEN.
        let with_taskbar = PxRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        assert_eq!(
            place_preview(None, with_taskbar, 760, 520),
            Placement {
                x: 580,
                y: 260,
                width: 760,
                height: 520,
            }
        );
    }

    #[test]
    fn monitor_at_negative_x() {
        let work = PxRect {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        let anchor = PxRect {
            left: -420,
            top: 700,
            right: 0,
            bottom: 1080,
        };
        let placed = place_preview(Some(anchor), work, 760, 520);
        assert_eq!(
            placed,
            Placement {
                x: -760,
                y: 560,
                width: 760,
                height: 520,
            }
        );
        assert_inside(work, placed);

        let middle = PxRect {
            left: -1200,
            top: 200,
            right: -800,
            bottom: 600,
        };
        let placed = place_preview(Some(middle), work, 760, 520);
        assert_eq!(
            placed,
            Placement {
                x: -1380,
                y: 140,
                width: 760,
                height: 520,
            }
        );
        assert_inside(work, placed);

        assert_eq!(
            place_preview(None, work, 760, 520),
            Placement {
                x: -1340,
                y: 280,
                width: 760,
                height: 520,
            }
        );
    }

    #[test]
    fn shrinks_when_work_area_is_smaller_than_the_preview() {
        let work = PxRect {
            left: 50,
            top: 40,
            right: 250,
            bottom: 200,
        };
        let placed = place_preview(
            Some(PxRect {
                left: 0,
                top: 0,
                right: 10,
                bottom: 10,
            }),
            work,
            LOGICAL_PREVIEW_WIDTH,
            LOGICAL_PREVIEW_HEIGHT,
        );
        assert_eq!(
            placed,
            Placement {
                x: 50,
                y: 40,
                width: 200,
                height: 160,
            }
        );
        assert_inside(work, placed);
        assert_eq!(place_preview(None, work, 760, 520), placed);
    }

    #[test]
    fn scales_logical_sizes_at_120_144_and_192() {
        assert_eq!(
            scaled_preview_size(96),
            PxSize {
                width: 760,
                height: 520,
            }
        );
        assert_eq!(scaled_font_height(96), 18);
        assert_eq!(scaled_font_height(0), 18);

        assert_eq!(scale_for_dpi(760, 120), 950);
        assert_eq!(scale_for_dpi(520, 120), 650);
        assert_eq!(scale_for_dpi(18, 120), 23);
        assert_eq!(
            scaled_preview_size(120),
            PxSize {
                width: 950,
                height: 650,
            }
        );
        assert_eq!(scaled_font_height(120), 23);

        assert_eq!(
            scaled_preview_size(144),
            PxSize {
                width: 1140,
                height: 780,
            }
        );
        assert_eq!(scaled_font_height(144), 27);

        assert_eq!(
            scaled_preview_size(192),
            PxSize {
                width: 1520,
                height: 1040,
            }
        );
        assert_eq!(scaled_font_height(192), 36);
    }

    #[test]
    fn dpi_scaled_bottom_right_anchor_stays_inside_work_area() {
        let work = work_1920x1080();
        let anchor = Some(bottom_right_anchor());
        assert_eq!(
            preview_placement(anchor, work, 120),
            Placement {
                x: 970,
                y: 430,
                width: 950,
                height: 650,
            }
        );
        assert_eq!(
            preview_placement(anchor, work, 144),
            Placement {
                x: 780,
                y: 300,
                width: 1140,
                height: 780,
            }
        );
        assert_eq!(
            preview_placement(anchor, work, 192),
            Placement {
                x: 400,
                y: 40,
                width: 1520,
                height: 1040,
            }
        );
        assert_inside(work, preview_placement(anchor, work, 120));
        assert_inside(work, preview_placement(anchor, work, 144));
        assert_inside(work, preview_placement(anchor, work, 192));
    }

    #[test]
    fn muldiv_matches_win32_rounding() {
        assert_eq!(muldiv(10, 10, 3), 33);
        assert_eq!(muldiv(-10, 10, 3), -33);
        assert_eq!(muldiv(18, 120, 96), 23);
        assert_eq!(muldiv(1, 1, 0), -1);
        assert_eq!(muldiv(i32::MAX, i32::MAX, 1), -1);
    }

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> PxRect {
        PxRect {
            left,
            top,
            right,
            bottom,
        }
    }

    fn min_320x200() -> PxSize {
        PxSize {
            width: 320,
            height: 200,
        }
    }

    /// 400×300 window centred at (300, 230). Sums are (600, 460).
    fn anchor_400x300() -> PxRect {
        rect(100, 80, 500, 380)
    }

    fn resize(anchor: PxRect, proposed: PxRect, grip: SizeGrip) -> PxRect {
        center_anchored_size(anchor, proposed, grip, min_320x200(), work_1920x1080())
    }

    fn assert_x_sum(anchor: PxRect, got: PxRect) {
        assert_eq!(
            i64::from(got.left) + i64::from(got.right),
            i64::from(anchor.left) + i64::from(anchor.right),
            "{got:?}"
        );
    }

    fn assert_y_sum(anchor: PxRect, got: PxRect) {
        assert_eq!(
            i64::from(got.top) + i64::from(got.bottom),
            i64::from(anchor.top) + i64::from(anchor.bottom),
            "{got:?}"
        );
    }

    fn assert_work_contains(work: PxRect, got: PxRect) {
        assert!(got.right > got.left && got.bottom > got.top, "{got:?}");
        assert!(got.left >= work.left && got.top >= work.top, "{got:?}");
        assert!(
            got.right <= work.right && got.bottom <= work.bottom,
            "{got:?} vs {work:?}"
        );
    }

    #[test]
    fn wmsz_values_map_onto_the_eight_grips() {
        assert_eq!(SizeGrip::from_wmsz(1), Some(SizeGrip::Left));
        assert_eq!(SizeGrip::from_wmsz(2), Some(SizeGrip::Right));
        assert_eq!(SizeGrip::from_wmsz(3), Some(SizeGrip::Top));
        assert_eq!(SizeGrip::from_wmsz(4), Some(SizeGrip::TopLeft));
        assert_eq!(SizeGrip::from_wmsz(5), Some(SizeGrip::TopRight));
        assert_eq!(SizeGrip::from_wmsz(6), Some(SizeGrip::Bottom));
        assert_eq!(SizeGrip::from_wmsz(7), Some(SizeGrip::BottomLeft));
        assert_eq!(SizeGrip::from_wmsz(8), Some(SizeGrip::BottomRight));
        assert_eq!(SizeGrip::from_wmsz(0), None);
        assert_eq!(SizeGrip::from_wmsz(9), None);
        assert_eq!(SizeGrip::from_wmsz(u32::MAX), None);
    }

    #[test]
    fn each_edge_and_corner_mirrors_around_the_centre() {
        let anchor = anchor_400x300();
        // Right +15 and left −15 are the same symmetric rect.
        let grown_x = rect(85, 80, 515, 380);
        assert_eq!(
            resize(anchor, rect(100, 80, 515, 380), SizeGrip::Right),
            grown_x
        );
        assert_eq!(
            resize(anchor, rect(85, 80, 500, 380), SizeGrip::Left),
            grown_x
        );
        assert_x_sum(anchor, grown_x);
        assert_eq!(grown_x.top, anchor.top);
        assert_eq!(grown_x.bottom, anchor.bottom);

        let shrunk_x = rect(130, 80, 470, 380);
        assert_eq!(
            resize(anchor, rect(130, 80, 500, 380), SizeGrip::Left),
            shrunk_x
        );
        assert_eq!(
            resize(anchor, rect(100, 80, 470, 380), SizeGrip::Right),
            shrunk_x
        );
        assert_x_sum(anchor, shrunk_x);

        let grown_y = rect(100, 68, 500, 392);
        assert_eq!(
            resize(anchor, rect(100, 68, 500, 380), SizeGrip::Top),
            grown_y
        );
        assert_eq!(
            resize(anchor, rect(100, 80, 500, 392), SizeGrip::Bottom),
            grown_y
        );
        assert_y_sum(anchor, grown_y);
        assert_eq!(grown_y.left, anchor.left);
        assert_eq!(grown_y.right, anchor.right);

        let shrunk_y = rect(100, 120, 500, 340);
        assert_eq!(
            resize(anchor, rect(100, 120, 500, 380), SizeGrip::Top),
            shrunk_y
        );
        assert_eq!(
            resize(anchor, rect(100, 80, 500, 340), SizeGrip::Bottom),
            shrunk_y
        );

        // Corners: each axis follows the pointer. Opposite sides match.
        assert_eq!(
            resize(anchor, rect(90, 74, 500, 380), SizeGrip::TopLeft),
            rect(90, 74, 510, 386)
        );
        assert_eq!(
            resize(anchor, rect(100, 76, 508, 380), SizeGrip::TopRight),
            rect(92, 76, 508, 384)
        );
        assert_eq!(
            resize(anchor, rect(94, 80, 500, 394), SizeGrip::BottomLeft),
            rect(94, 66, 506, 394)
        );
        assert_eq!(
            resize(anchor, rect(100, 80, 511, 387), SizeGrip::BottomRight),
            rect(89, 73, 511, 387)
        );

        // Equal pointer deltas move all four sides by that same amount.
        let square = rect(100, 100, 500, 400);
        let uniform = resize(square, rect(100, 100, 525, 425), SizeGrip::BottomRight);
        assert_eq!(uniform, rect(75, 75, 525, 425));
        assert_eq!(uniform.left, square.left - 25);
        assert_eq!(uniform.right, square.right + 25);
        assert_eq!(uniform.top, square.top - 25);
        assert_eq!(uniform.bottom, square.bottom + 25);
    }

    #[test]
    fn a_drag_that_does_not_move_keeps_the_anchor() {
        let anchor = anchor_400x300();
        for grip in [
            SizeGrip::Left,
            SizeGrip::Right,
            SizeGrip::Top,
            SizeGrip::Bottom,
            SizeGrip::TopLeft,
            SizeGrip::TopRight,
            SizeGrip::BottomLeft,
            SizeGrip::BottomRight,
        ] {
            assert_eq!(resize(anchor, anchor, grip), anchor, "{grip:?}");
        }
    }

    #[test]
    fn min_size_grows_around_the_centre() {
        let anchor = anchor_400x300();
        // Dragged past the centre. Width would invert; clamp to 320, centred.
        let wide = resize(anchor, rect(100, 80, 150, 380), SizeGrip::Right);
        assert_eq!(wide, rect(140, 80, 460, 380));
        assert_x_sum(anchor, wide);
        assert_eq!(wide.right - wide.left, 320);
        assert_eq!(wide.top, anchor.top);
        assert_eq!(wide.bottom, anchor.bottom);

        let tall = resize(anchor, rect(100, 400, 500, 380), SizeGrip::Top);
        assert_eq!(tall, rect(100, 130, 500, 330));
        assert_y_sum(anchor, tall);
        assert_eq!(tall.bottom - tall.top, 200);
        assert_eq!(tall.left, anchor.left);

        let both = resize(anchor, rect(450, 350, 500, 380), SizeGrip::TopLeft);
        assert_eq!(both, rect(140, 130, 460, 330));
        assert_eq!(both.right - both.left, 320);
        assert_eq!(both.bottom - both.top, 200);
        assert_x_sum(anchor, both);
        assert_y_sum(anchor, both);

        // Same proposed rect twice: the half-pixel choice does not walk.
        assert_eq!(
            resize(anchor, rect(100, 80, 150, 380), SizeGrip::Right),
            wide
        );
    }

    #[test]
    fn odd_min_keeps_the_centre_and_even_min_shifts_half_a_pixel() {
        // Room on every side, so the work area does not also shift the rect.
        let anchor = rect(100, 80, 201, 380);
        let work = rect(-4_000, -4_000, 4_000, 4_000);
        let proposed = rect(100, 80, 50, 380);
        assert_eq!(anchor.left + anchor.right, 301);
        let odd_min = PxSize {
            width: 321,
            height: 200,
        };
        let odd = center_anchored_size(anchor, proposed, SizeGrip::Right, odd_min, work);
        assert_eq!(odd, rect(-10, 80, 311, 380));
        assert_eq!(odd.left + odd.right, 301);

        let even = center_anchored_size(anchor, proposed, SizeGrip::Right, min_320x200(), work);
        // Floor of the half-pixel centre moves it toward the low edge by 0.5.
        assert_eq!(even, rect(-10, 80, 310, 380));
        assert_eq!(even.left + even.right, 300);
        assert_eq!(
            center_anchored_size(anchor, proposed, SizeGrip::Right, min_320x200(), work),
            even
        );
    }

    #[test]
    fn work_area_clamp_keeps_each_side_centred_inside() {
        let work = work_1920x1080();

        let near_left = rect(20, 100, 420, 400);
        let left = resize(near_left, rect(20, 100, 1500, 400), SizeGrip::Right);
        assert_eq!(left, rect(0, 100, 440, 400));
        assert_x_sum(near_left, left);
        assert_work_contains(work, left);

        let near_right = rect(1500, 100, 1900, 400);
        let right = resize(near_right, rect(1500, 100, 2100, 400), SizeGrip::Right);
        assert_eq!(right, rect(1480, 100, 1920, 400));
        assert_x_sum(near_right, right);
        assert_work_contains(work, right);

        let near_top = rect(400, 15, 800, 315);
        let top = resize(near_top, rect(400, -80, 800, 315), SizeGrip::Top);
        assert_eq!(top, rect(400, 0, 800, 330));
        assert_y_sum(near_top, top);
        assert_work_contains(work, top);

        let near_bottom = rect(400, 700, 800, 1000);
        let bottom = resize(near_bottom, rect(400, 700, 800, 1300), SizeGrip::Bottom);
        assert_eq!(bottom, rect(400, 620, 800, 1080));
        assert_y_sum(near_bottom, bottom);
        assert_work_contains(work, bottom);

        // A corner stops on the tight axis and keeps mirroring the other.
        let corner_anchor = rect(20, 200, 420, 500);
        let corner = resize(
            corner_anchor,
            rect(20, 200, 1500, 560),
            SizeGrip::BottomRight,
        );
        assert_eq!(corner, rect(0, 140, 440, 560));
        assert_x_sum(corner_anchor, corner);
        assert_y_sum(corner_anchor, corner);
        assert_work_contains(work, corner);

        // Negative monitor origin. The right edge of the work area is 0.
        let neg_work = rect(-1920, 0, 0, 1080);
        let neg_anchor = rect(-400, 100, -100, 400);
        let neg = center_anchored_size(
            neg_anchor,
            rect(-1800, 100, -100, 400),
            SizeGrip::Left,
            min_320x200(),
            neg_work,
        );
        assert_eq!(neg, rect(-500, 100, 0, 400));
        assert_x_sum(neg_anchor, neg);
        assert_work_contains(neg_work, neg);
    }

    #[test]
    fn minimum_that_cannot_stay_centred_shifts_inside_the_work_area() {
        let work = work_1920x1080();

        // Centre is 60px from the left. Half of 320 is 160, so symmetry breaks.
        let near_left = rect(10, 200, 110, 500);
        let left = resize(near_left, rect(10, 200, 400, 500), SizeGrip::Right);
        assert_eq!(left, rect(0, 200, 320, 500));
        assert_eq!(left.right - left.left, 320);
        assert_ne!(left.left + left.right, near_left.left + near_left.right);
        assert_work_contains(work, left);

        let near_right = rect(1810, 200, 1910, 500);
        let right = resize(near_right, rect(1810, 200, 2000, 500), SizeGrip::Right);
        assert_eq!(right, rect(1600, 200, 1920, 500));
        assert_work_contains(work, right);

        let near_top = rect(400, 5, 800, 105);
        let top = resize(near_top, rect(400, -40, 800, 105), SizeGrip::Top);
        assert_eq!(top, rect(400, 0, 800, 200));
        assert_eq!(top.bottom - top.top, 200);
        assert_work_contains(work, top);

        let near_bottom = rect(400, 980, 800, 1060);
        let bottom = resize(near_bottom, rect(400, 980, 800, 1200), SizeGrip::Bottom);
        assert_eq!(bottom, rect(400, 880, 800, 1080));
        assert_work_contains(work, bottom);

        // Work area smaller than the minimum: pin that axis to the work span.
        let tiny = rect(100, 100, 250, 220);
        let anchor = rect(120, 110, 200, 180);
        let edge = center_anchored_size(
            anchor,
            rect(120, 110, 230, 180),
            SizeGrip::Right,
            min_320x200(),
            tiny,
        );
        assert_eq!(edge, rect(100, 110, 250, 180));
        assert_eq!(edge.top, anchor.top);
        assert_eq!(edge.bottom, anchor.bottom);

        let corner = center_anchored_size(
            anchor,
            rect(120, 110, 230, 200),
            SizeGrip::BottomRight,
            min_320x200(),
            tiny,
        );
        assert_eq!(corner, rect(100, 100, 250, 220));
    }

    #[test]
    fn undragged_axis_stays_put_even_when_it_is_outside_the_work_area() {
        let anchor = rect(100, -40, 500, 20);
        let got = resize(anchor, rect(100, -40, 560, 20), SizeGrip::Right);
        assert_eq!(got, rect(40, -40, 560, 20));
        assert_eq!(got.top, anchor.top);
        assert_eq!(got.bottom, anchor.bottom);
        assert_x_sum(anchor, got);
    }

    #[test]
    fn odd_pixel_deltas_do_not_drift_over_many_steps() {
        let anchor = rect(100, 50, 201, 151);
        assert_eq!(anchor.right - anchor.left, 101);
        let min = PxSize {
            width: 40,
            height: 40,
        };
        let work = rect(-10_000, -10_000, 10_000, 10_000);
        let mut current = anchor;
        for step in 1..=64 {
            let proposed = rect(
                current.left,
                current.top,
                current.right + 1,
                current.bottom + 1,
            );
            current = center_anchored_size(anchor, proposed, SizeGrip::BottomRight, min, work);
            assert_eq!(current.left + current.right, 301, "step {step}");
            assert_eq!(current.top + current.bottom, 201, "step {step}");
            assert_eq!(current.right - current.left, 101 + 2 * step, "step {step}");
            assert_eq!(current.bottom - current.top, 101 + 2 * step, "step {step}");
        }

        let mut current = anchor;
        for step in 1..=20 {
            let proposed = rect(current.left - 3, current.top, current.right, current.bottom);
            current = center_anchored_size(anchor, proposed, SizeGrip::Left, min, work);
            assert_eq!(current.left + current.right, 301, "step {step}");
            assert_eq!(current.right - current.left, 101 + 6 * step, "step {step}");
            assert_eq!(current.top, anchor.top);
            assert_eq!(current.bottom, anchor.bottom);
        }

        // Negative origin, same rule.
        let anchor = rect(-500, 40, -399, 140);
        assert_eq!(anchor.left + anchor.right, -899);
        let mut current = anchor;
        for step in 1..=40 {
            let proposed = rect(current.left, current.top - 1, current.right, current.bottom);
            current = center_anchored_size(anchor, proposed, SizeGrip::Top, min, work);
            assert_eq!(current.left + current.right, -899, "step {step}");
            assert_eq!(current.top + current.bottom, 180, "step {step}");
            assert_eq!(current.bottom - current.top, 100 + 2 * step, "step {step}");
        }

        // Once a side hits the work area, further drags stay on that rect.
        let anchor = rect(20, 100, 420, 400);
        let mut right = 420;
        for _ in 0..30 {
            right += 1;
            let got = resize(anchor, rect(20, 100, right, 400), SizeGrip::Right);
            assert_eq!(got.left + got.right, 440);
            assert_eq!(got.top, 100);
            assert_eq!(got.bottom, 400);
            assert_work_contains(work_1920x1080(), got);
        }
        assert_eq!(
            resize(anchor, rect(20, 100, 520, 400), SizeGrip::Right),
            rect(0, 100, 440, 400)
        );
    }

    #[test]
    fn degenerate_anchor_and_empty_work_area_are_safe() {
        let proposed = rect(10, 20, 30, 40);
        assert_eq!(
            resize(rect(5, 5, 5, 40), proposed, SizeGrip::Right),
            proposed
        );
        assert_eq!(
            resize(rect(5, 5, 40, 5), proposed, SizeGrip::Bottom),
            proposed
        );

        // No work-area span: mirror only, do not clamp.
        let anchor = anchor_400x300();
        let got = center_anchored_size(
            anchor,
            rect(100, 80, 515, 380),
            SizeGrip::Right,
            min_320x200(),
            rect(0, 0, 0, 0),
        );
        assert_eq!(got, rect(85, 80, 515, 380));

        let extreme = resize(anchor, rect(100, 80, i32::MAX, 380), SizeGrip::Right);
        assert_eq!(extreme, rect(0, 80, 600, 380));
        assert_work_contains(work_1920x1080(), extreme);

        let inverted = resize(anchor, rect(100, 80, i32::MIN, 380), SizeGrip::Right);
        assert_eq!(inverted, rect(140, 80, 460, 380));
        assert_work_contains(work_1920x1080(), inverted);
    }
}
