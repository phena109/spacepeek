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
}
