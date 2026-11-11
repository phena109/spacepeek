//! Preview-window chrome that does not touch Win32.
//!
//! Header copy, hit testing, and the remembered size are pure so they can be
//! tested on any host. The Windows window procedure only maps the results.

use std::path::Path;
use std::sync::Arc;

use crate::geometry::{
    effective_dpi, muldiv, scale_for_dpi, PxRect, PxSize, BASE_DPI, LOGICAL_PREVIEW_HEIGHT,
    LOGICAL_PREVIEW_WIDTH,
};
use crate::image_preview::{format_image_details, DecodedImage};
use crate::preview::{format_compact_size, format_grouped, preview_text, Preview, PreviewError};

/// Logical height of the header strip at 96 DPI.
pub const LOGICAL_HEADER_HEIGHT: i32 = 52;
/// Logical width of the × button at 96 DPI.
pub const LOGICAL_CLOSE_WIDTH: i32 = 44;
/// Logical height of the header name font at 96 DPI.
pub const LOGICAL_HEADER_FONT: i32 = 16;
/// Logical height of the header details font at 96 DPI.
pub const LOGICAL_DETAIL_FONT: i32 = 13;
/// Logical horizontal padding inside the header at 96 DPI.
pub const LOGICAL_HEADER_PAD_X: i32 = 12;
/// Smallest outer width the user can drag to, in logical pixels.
pub const MIN_LOGICAL_WIDTH: i32 = 320;
/// Smallest outer height the user can drag to, in logical pixels.
pub const MIN_LOGICAL_HEIGHT: i32 = 200;
/// Outer size of a message window before the user resizes, in logical pixels.
pub const MESSAGE_LOGICAL_WIDTH: i32 = 480;
pub const MESSAGE_LOGICAL_HEIGHT: i32 = 240;

/// Outer window size in logical (96 DPI) pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalSize {
    pub width: i32,
    pub height: i32,
}

/// What the body of the preview is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowKind {
    /// Monospace file text.
    Content,
    /// Centred error, folder, shortcut, or empty-file message.
    Message,
    /// PNG or JPEG bitmap. Opens at the same size as [`Self::Content`].
    Image,
}

/// File name and the secondary header line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderText {
    pub name: String,
    pub details: String,
}

/// What the window should draw. The text body is the edit control; a message
/// is painted centred under the header. An image is the decoded bitmap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presented {
    Text {
        header: HeaderText,
        body: String,
    },
    Message {
        header: HeaderText,
        message: String,
    },
    Image {
        header: HeaderText,
        image: Arc<DecodedImage>,
    },
}

impl Presented {
    pub fn kind(&self) -> WindowKind {
        match self {
            Presented::Text { .. } => WindowKind::Content,
            Presented::Message { .. } => WindowKind::Message,
            Presented::Image { .. } => WindowKind::Image,
        }
    }

    pub fn header(&self) -> &HeaderText {
        match self {
            Presented::Text { header, .. }
            | Presented::Message { header, .. }
            | Presented::Image { header, .. } => header,
        }
    }

    pub fn message(&self) -> Option<&str> {
        match self {
            Presented::Message { message, .. } => Some(message),
            Presented::Text { .. } | Presented::Image { .. } => None,
        }
    }
}

/// Header details such as `12 KB · 2,000 lines (truncated)`.
///
/// Sizes are binary units with a one-decimal remainder (`1.5 KB`, `1.2 MB`).
/// Line counts use thousands separators. `truncated` is the preview's existing
/// truncation flag, not a second measurement.
pub fn format_header_details(total_bytes: u64, line_count: u64, truncated: bool) -> String {
    let lines = if line_count == 1 {
        "1 line".to_string()
    } else {
        format!("{} lines", format_grouped(line_count))
    };
    let mut details = format!("{} · {lines}", format_compact_size(total_bytes));
    if truncated {
        details.push_str(" (truncated)");
    }
    details
}

/// File name shown in the header. An empty path becomes `This file`.
pub fn path_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| {
            let text = path.to_string_lossy();
            if text.is_empty() {
                "This file".to_string()
            } else {
                text.into_owned()
            }
        })
}

pub fn chrome_for_text(path: &Path, preview: &Preview) -> Presented {
    if let Some(image) = preview.image.as_ref() {
        return Presented::Image {
            header: HeaderText {
                name: path_label(path),
                details: format_image_details(preview.total_bytes, image),
            },
            image: Arc::clone(image),
        };
    }
    Presented::Text {
        header: HeaderText {
            name: path_label(path),
            details: format_header_details(
                preview.total_bytes,
                preview.line_count as u64,
                preview.truncated,
            ),
        },
        body: preview_text(preview),
    }
}

pub fn chrome_for_error(path: &Path, error: &PreviewError) -> Presented {
    let name = path_label(path);
    Presented::Message {
        header: HeaderText {
            name: name.clone(),
            details: error.header_status(),
        },
        message: error.user_message(&name),
    }
}

pub fn default_logical_size(kind: WindowKind) -> LogicalSize {
    match kind {
        WindowKind::Content | WindowKind::Image => LogicalSize {
            width: LOGICAL_PREVIEW_WIDTH,
            height: LOGICAL_PREVIEW_HEIGHT,
        },
        WindowKind::Message => LogicalSize {
            width: MESSAGE_LOGICAL_WIDTH,
            height: MESSAGE_LOGICAL_HEIGHT,
        },
    }
}

pub fn clamp_logical_size(size: LogicalSize) -> LogicalSize {
    LogicalSize {
        width: size.width.max(MIN_LOGICAL_WIDTH),
        height: size.height.max(MIN_LOGICAL_HEIGHT),
    }
}

fn unscale_for_dpi(value: i32, dpi: u32) -> i32 {
    let dpi = effective_dpi(dpi);
    let dpi_i = i32::try_from(dpi).unwrap_or(i32::MAX);
    muldiv(value, BASE_DPI, dpi_i)
}

pub fn physical_to_logical(width: i32, height: i32, dpi: u32) -> LogicalSize {
    LogicalSize {
        width: unscale_for_dpi(width, dpi),
        height: unscale_for_dpi(height, dpi),
    }
}

pub fn logical_to_physical(size: LogicalSize, dpi: u32) -> PxSize {
    let dpi = effective_dpi(dpi);
    PxSize {
        width: scale_for_dpi(size.width, dpi),
        height: scale_for_dpi(size.height, dpi),
    }
}

pub fn scaled_header_height(dpi: u32) -> i32 {
    scale_for_dpi(LOGICAL_HEADER_HEIGHT, effective_dpi(dpi))
}

pub fn scaled_close_width(dpi: u32) -> i32 {
    scale_for_dpi(LOGICAL_CLOSE_WIDTH, effective_dpi(dpi))
}

pub fn scaled_header_font(dpi: u32) -> i32 {
    scale_for_dpi(LOGICAL_HEADER_FONT, effective_dpi(dpi))
}

pub fn scaled_detail_font(dpi: u32) -> i32 {
    scale_for_dpi(LOGICAL_DETAIL_FONT, effective_dpi(dpi))
}

pub fn scaled_header_pad(dpi: u32) -> i32 {
    scale_for_dpi(LOGICAL_HEADER_PAD_X, effective_dpi(dpi))
}

pub fn min_physical_size(dpi: u32) -> PxSize {
    logical_to_physical(
        LogicalSize {
            width: MIN_LOGICAL_WIDTH,
            height: MIN_LOGICAL_HEIGHT,
        },
        dpi,
    )
}

/// Last user-chosen outer size, in logical pixels, for this process.
///
/// Nothing is stored until the user finishes a resize. Message and text
/// previews share one size after that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeMemory {
    chosen: Option<LogicalSize>,
}

impl SizeMemory {
    pub const fn new() -> Self {
        Self { chosen: None }
    }

    pub const fn chosen(self) -> Option<LogicalSize> {
        self.chosen
    }

    /// Store `size`, clamped up to the minimum. Returns the stored value.
    pub fn remember(&mut self, size: LogicalSize) -> LogicalSize {
        let size = clamp_logical_size(size);
        self.chosen = Some(size);
        size
    }

    /// Convert an outer physical size at `dpi` and store it.
    ///
    /// Non-positive sizes are ignored so a failed measurement cannot replace
    /// a real choice with the minimum.
    pub fn remember_physical(&mut self, width: i32, height: i32, dpi: u32) -> Option<LogicalSize> {
        if width <= 0 || height <= 0 {
            return None;
        }
        Some(self.remember(physical_to_logical(width, height, dpi)))
    }

    pub fn opening_logical(self, kind: WindowKind) -> LogicalSize {
        clamp_logical_size(self.chosen.unwrap_or_else(|| default_logical_size(kind)))
    }

    pub fn opening_physical(self, kind: WindowKind, dpi: u32) -> PxSize {
        logical_to_physical(self.opening_logical(kind), dpi)
    }
}

impl Default for SizeMemory {
    fn default() -> Self {
        Self::new()
    }
}

/// Client-area rectangle of the × button.
///
/// `None` when the button has no pixels. The origin is the client origin,
/// so this is the rect to invalidate — not an outer-window [`hit_test`] point.
pub fn close_button_bounds(
    client_width: i32,
    header_height: i32,
    close_width: i32,
) -> Option<PxRect> {
    if client_width <= 0 || header_height <= 0 || close_width <= 0 {
        return None;
    }
    let width = close_width.min(client_width);
    Some(PxRect {
        left: client_width.saturating_sub(width),
        top: 0,
        right: client_width,
        bottom: header_height,
    })
}

/// True when `(x, y)`, in outer-window pixels, is on the × button.
pub fn pointer_over_close(layout: ChromeLayout, x: i32, y: i32) -> bool {
    hit_test(layout, x, y) == HitZone::Close
}

/// What a left double-click in `zone` should do.
///
/// The body opens the file. The header, the × button, and the resize edges
/// do not. A triple-click is a later button-down, not a second one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoubleClickAction {
    /// Close the preview and open the file with the shell's default action.
    Open,
    /// Leave the preview alone.
    Ignore,
}

pub fn double_click_action(zone: HitZone) -> DoubleClickAction {
    match zone {
        HitZone::Client => DoubleClickAction::Open,
        HitZone::Caption
        | HitZone::Close
        | HitZone::Left
        | HitZone::Right
        | HitZone::Top
        | HitZone::Bottom
        | HitZone::TopLeft
        | HitZone::TopRight
        | HitZone::BottomLeft
        | HitZone::BottomRight => DoubleClickAction::Ignore,
    }
}

/// Where a point in outer-window coordinates lands.
///
/// Edges are the non-client frame. The header and the × button sit in the
/// client area under that frame, so a point in the top frame is a resize,
/// not a drag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitZone {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    /// Header, excluding the close button. Drag the window.
    Caption,
    /// The × button.
    Close,
    /// Edit control or centred message.
    Client,
}

/// Outer size plus the frame and header metrics, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromeLayout {
    pub width: i32,
    pub height: i32,
    pub frame_left: i32,
    pub frame_top: i32,
    pub frame_right: i32,
    pub frame_bottom: i32,
    pub header_height: i32,
    pub close_width: i32,
}

pub fn hit_test(layout: ChromeLayout, x: i32, y: i32) -> HitZone {
    if layout.width <= 0 || layout.height <= 0 {
        return HitZone::Client;
    }
    let left = layout.frame_left.max(0);
    let top = layout.frame_top.max(0);
    let right = layout.frame_right.max(0);
    let bottom = layout.frame_bottom.max(0);
    let on_left = x < left;
    let on_right = x >= layout.width.saturating_sub(right);
    let on_top = y < top;
    let on_bottom = y >= layout.height.saturating_sub(bottom);
    match (on_left, on_right, on_top, on_bottom) {
        (true, _, true, _) => return HitZone::TopLeft,
        (_, true, true, _) => return HitZone::TopRight,
        (true, _, _, true) => return HitZone::BottomLeft,
        (_, true, _, true) => return HitZone::BottomRight,
        (true, _, _, _) => return HitZone::Left,
        (_, true, _, _) => return HitZone::Right,
        (_, _, true, _) => return HitZone::Top,
        (_, _, _, true) => return HitZone::Bottom,
        _ => {}
    }

    let client_w = layout.width.saturating_sub(left).saturating_sub(right);
    let header = layout.header_height.max(0);
    let close = layout.close_width.max(0);
    let client_x = x.saturating_sub(left);
    let client_y = y.saturating_sub(top);
    if client_w > 0
        && header > 0
        && (0..header).contains(&client_y)
        && (0..client_w).contains(&client_x)
    {
        let close_left = client_w.saturating_sub(close);
        if close > 0 && client_x >= close_left {
            HitZone::Close
        } else {
            HitZone::Caption
        }
    } else {
        HitZone::Client
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::{Preview, Truncation};
    use std::path::PathBuf;

    fn sample_layout() -> ChromeLayout {
        ChromeLayout {
            width: 800,
            height: 600,
            frame_left: 8,
            frame_top: 8,
            frame_right: 8,
            frame_bottom: 8,
            header_height: 56,
            close_width: 44,
        }
    }

    fn preview(total_bytes: u64, line_count: usize, truncated: bool) -> Preview {
        Preview {
            content: "body".to_string(),
            truncated,
            line_count,
            truncation: None,
            total_bytes,
            image: None,
        }
    }

    #[test]
    fn header_details_format_sizes_lines_and_truncation() {
        assert_eq!(format_header_details(0, 0, false), "0 B · 0 lines");
        assert_eq!(format_header_details(1, 1, false), "1 B · 1 line");
        assert_eq!(format_header_details(512, 3, false), "512 B · 3 lines");
        assert_eq!(
            format_header_details(12 * 1024, 40, false),
            "12 KB · 40 lines"
        );
        assert_eq!(format_header_details(1536, 2, false), "1.5 KB · 2 lines");
        assert_eq!(
            format_header_details(1024 * 1024, 8, false),
            "1 MB · 8 lines"
        );
        assert_eq!(
            format_header_details(1_258_291, 2_000, true),
            "1.2 MB · 2,000 lines (truncated)"
        );
        assert_eq!(
            format_header_details(100, 48_113, false),
            "100 B · 48,113 lines"
        );
        assert_eq!(
            format_header_details(64 * 1024, 2_000, true),
            "64 KB · 2,000 lines (truncated)"
        );
        assert_eq!(
            format_header_details(5 * 1024 * 1024, 1, true),
            "5 MB · 1 line (truncated)"
        );
    }

    #[test]
    fn header_keeps_non_ascii_names_and_reuses_truncation() {
        let path = PathBuf::from("dir").join("ノート.md");
        let mut shown = preview(1_258_291, 2_000, true);
        shown.content = "line\r\n".to_string();
        shown.truncation = Some(Truncation {
            bytes_shown: 64 * 1024,
            total_bytes: 1_258_291,
            lines_shown: 2_000,
            total_lines: None,
            lines_omitted: true,
            lines_shortened: false,
        });
        let presented = chrome_for_text(&path, &shown);
        let Presented::Text { header, body } = presented else {
            panic!("expected text chrome");
        };
        assert_eq!(header.name, "ノート.md");
        assert_eq!(header.details, "1.2 MB · 2,000 lines (truncated)");
        assert!(body.starts_with("Showing the first 64 KB of 1.2 MB"));
        assert!(body.contains("line"));

        let plain = chrome_for_text(Path::new("notes.md"), &preview(12 * 1024, 40, false));
        assert_eq!(
            plain.header(),
            &HeaderText {
                name: "notes.md".to_string(),
                details: "12 KB · 40 lines".to_string(),
            }
        );
        assert_eq!(plain.kind(), WindowKind::Content);
        assert_eq!(plain.message(), None);
    }

    #[test]
    fn image_preview_uses_source_dimensions_and_the_content_window() {
        use crate::image_preview::{DecodedImage, ImageFormat};
        use std::sync::Arc;

        let image = Arc::new(DecodedImage {
            width: 800,
            height: 600,
            source_width: 4_000,
            source_height: 3_000,
            format: ImageFormat::Jpeg,
            bgra: vec![0, 0, 0, 255],
        });
        let preview = Preview {
            content: String::new(),
            truncated: false,
            line_count: 0,
            truncation: None,
            total_bytes: 1_258_291,
            image: Some(Arc::clone(&image)),
        };
        let presented = chrome_for_text(Path::new("photo.jpg"), &preview);
        assert_eq!(presented.kind(), WindowKind::Image);
        assert_eq!(presented.message(), None);
        assert_eq!(presented.header().name, "photo.jpg");
        assert_eq!(presented.header().details, "1.2 MB · 4,000 × 3,000 · JPEG");
        let Presented::Image { image: shown, .. } = presented else {
            panic!("expected image chrome");
        };
        assert!(Arc::ptr_eq(&shown, &image));
        assert_eq!(
            default_logical_size(WindowKind::Image),
            LogicalSize {
                width: 760,
                height: 520
            }
        );
        let mem = SizeMemory::new();
        assert_eq!(
            mem.opening_logical(WindowKind::Image),
            mem.opening_logical(WindowKind::Content)
        );
    }

    #[test]
    fn error_chrome_is_a_centred_message() {
        let presented = chrome_for_error(Path::new("empty.txt"), &PreviewError::Empty);
        assert_eq!(
            presented,
            Presented::Message {
                header: HeaderText {
                    name: "empty.txt".to_string(),
                    details: "0 B".to_string(),
                },
                message: "This file is empty".to_string(),
            }
        );
        assert_eq!(presented.kind(), WindowKind::Message);

        let folder = PathBuf::from("docs").join("写真");
        let presented = chrome_for_error(
            &folder,
            &PreviewError::Directory {
                count: Some(crate::preview::DirectoryCount::Exact(12)),
            },
        );
        assert_eq!(presented.header().name, "写真");
        assert_eq!(presented.header().details, "12 items");
        assert_eq!(presented.message(), Some("Folder: 写真 · 12 items"));

        let missing = chrome_for_error(
            Path::new("café.txt"),
            &PreviewError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "missing")),
        );
        assert_eq!(missing.header().name, "café.txt");
        assert_eq!(missing.message(), Some("café.txt no longer exists"));
    }

    #[test]
    fn hit_test_regions() {
        let layout = sample_layout();
        // Client width is 784. Close starts at 784 - 44 = 740 in client coords,
        // which is 748 in window coords.
        assert_eq!(hit_test(layout, 100, 30), HitZone::Caption);
        assert_eq!(hit_test(layout, 747, 30), HitZone::Caption);
        assert_eq!(hit_test(layout, 748, 30), HitZone::Close);
        assert_eq!(hit_test(layout, 780, 60), HitZone::Close);
        assert_eq!(hit_test(layout, 100, 8 + 56), HitZone::Client);
        assert_eq!(hit_test(layout, 400, 300), HitZone::Client);

        assert_eq!(hit_test(layout, 2, 300), HitZone::Left);
        assert_eq!(hit_test(layout, 796, 300), HitZone::Right);
        assert_eq!(hit_test(layout, 400, 2), HitZone::Top);
        assert_eq!(hit_test(layout, 400, 596), HitZone::Bottom);
        assert_eq!(hit_test(layout, 2, 2), HitZone::TopLeft);
        assert_eq!(hit_test(layout, 796, 2), HitZone::TopRight);
        assert_eq!(hit_test(layout, 2, 596), HitZone::BottomLeft);
        assert_eq!(hit_test(layout, 796, 596), HitZone::BottomRight);

        // The frame wins over the header, including the × button. The right
        // frame starts at x = 792.
        assert_eq!(hit_test(layout, 3, 30), HitZone::Left);
        assert_eq!(hit_test(layout, 100, 4), HitZone::Top);
        assert_eq!(hit_test(layout, 793, 30), HitZone::Right);

        assert_eq!(
            hit_test(
                ChromeLayout {
                    width: 0,
                    height: 10,
                    ..layout
                },
                0,
                0
            ),
            HitZone::Client
        );
    }

    #[test]
    fn remembered_size_is_logical_and_clamped() {
        let mem = SizeMemory::new();
        assert_eq!(mem.chosen(), None);
        assert_eq!(
            mem.opening_logical(WindowKind::Content),
            LogicalSize {
                width: 760,
                height: 520
            }
        );
        assert_eq!(
            mem.opening_logical(WindowKind::Message),
            LogicalSize {
                width: 480,
                height: 240
            }
        );
        assert_eq!(
            mem.opening_physical(WindowKind::Content, 96),
            PxSize {
                width: 760,
                height: 520
            }
        );
        assert_eq!(
            mem.opening_physical(WindowKind::Content, 120),
            PxSize {
                width: 950,
                height: 650
            }
        );
        assert_eq!(
            mem.opening_physical(WindowKind::Message, 120),
            PxSize {
                width: 600,
                height: 300
            }
        );
        assert_eq!(
            mem.opening_physical(WindowKind::Content, 0),
            PxSize {
                width: 760,
                height: 520
            }
        );

        let mut mem = SizeMemory::new();
        assert_eq!(
            mem.remember(LogicalSize {
                width: 900,
                height: 600
            }),
            LogicalSize {
                width: 900,
                height: 600
            }
        );
        // A later message preview reuses the user's size.
        assert_eq!(
            mem.opening_logical(WindowKind::Message),
            LogicalSize {
                width: 900,
                height: 600
            }
        );
        assert_eq!(
            mem.remember(LogicalSize {
                width: 10,
                height: -5
            }),
            LogicalSize {
                width: MIN_LOGICAL_WIDTH,
                height: MIN_LOGICAL_HEIGHT
            }
        );

        let mut mem = SizeMemory::new();
        assert_eq!(
            mem.remember_physical(950, 650, 120),
            Some(LogicalSize {
                width: 760,
                height: 520
            })
        );
        assert_eq!(
            mem.opening_physical(WindowKind::Content, 96),
            PxSize {
                width: 760,
                height: 520
            }
        );
        assert_eq!(
            mem.opening_physical(WindowKind::Content, 120),
            PxSize {
                width: 950,
                height: 650
            }
        );
        assert_eq!(
            mem.opening_physical(WindowKind::Content, 0),
            mem.opening_physical(WindowKind::Content, 96)
        );

        // Just under the minimum at 120 DPI clamps up in logical pixels.
        assert_eq!(
            mem.remember_physical(399, 250, 120),
            Some(LogicalSize {
                width: MIN_LOGICAL_WIDTH,
                height: MIN_LOGICAL_HEIGHT
            })
        );
        let kept = mem.chosen();
        assert_eq!(mem.remember_physical(0, 100, 96), None);
        assert_eq!(mem.remember_physical(-1, 400, 96), None);
        assert_eq!(mem.chosen(), kept);

        assert_eq!(
            min_physical_size(96),
            PxSize {
                width: MIN_LOGICAL_WIDTH,
                height: MIN_LOGICAL_HEIGHT
            }
        );
        assert_eq!(
            min_physical_size(120),
            PxSize {
                width: 400,
                height: 250
            }
        );
        assert_eq!(scaled_header_height(96), LOGICAL_HEADER_HEIGHT);
        assert_eq!(scaled_header_height(0), LOGICAL_HEADER_HEIGHT);
        assert_eq!(scaled_header_height(120), 65);
        assert_eq!(scaled_close_width(120), 55);
        assert_eq!(scaled_header_font(120), 20);
        assert_eq!(scaled_detail_font(120), 16);
        assert_eq!(scaled_header_pad(96), LOGICAL_HEADER_PAD_X);
    }

    #[test]
    fn pointer_over_close_is_only_the_button() {
        let layout = sample_layout();
        // Same points as `hit_test_regions`: close starts at window x = 748.
        assert!(pointer_over_close(layout, 748, 30));
        assert!(pointer_over_close(layout, 780, 60));
        assert!(!pointer_over_close(layout, 747, 30));
        assert!(!pointer_over_close(layout, 100, 30));
        assert!(!pointer_over_close(layout, 400, 300));
        assert!(!pointer_over_close(layout, 100, 8 + 56));
        // The frame wins, including the strip beside the ×.
        assert!(!pointer_over_close(layout, 793, 30));
        assert!(!pointer_over_close(layout, 3, 30));
        assert!(!pointer_over_close(layout, 100, 4));
        assert!(!pointer_over_close(
            ChromeLayout {
                width: 0,
                height: 10,
                ..layout
            },
            0,
            0
        ));
    }

    #[test]
    fn close_button_bounds_follow_the_client_edge() {
        assert_eq!(
            close_button_bounds(784, 56, 44),
            Some(PxRect {
                left: 740,
                top: 0,
                right: 784,
                bottom: 56,
            })
        );
        assert_eq!(
            close_button_bounds(20, 56, 44),
            Some(PxRect {
                left: 0,
                top: 0,
                right: 20,
                bottom: 56,
            })
        );
        assert_eq!(close_button_bounds(0, 56, 44), None);
        assert_eq!(close_button_bounds(100, 0, 44), None);
        assert_eq!(close_button_bounds(100, 56, 0), None);
        assert_eq!(close_button_bounds(-1, 56, 44), None);
    }

    #[test]
    fn double_click_opens_only_the_body() {
        assert_eq!(
            double_click_action(HitZone::Client),
            DoubleClickAction::Open
        );
        for zone in [
            HitZone::Caption,
            HitZone::Close,
            HitZone::Left,
            HitZone::Right,
            HitZone::Top,
            HitZone::Bottom,
            HitZone::TopLeft,
            HitZone::TopRight,
            HitZone::BottomLeft,
            HitZone::BottomRight,
        ] {
            assert_eq!(
                double_click_action(zone),
                DoubleClickAction::Ignore,
                "{zone:?}"
            );
        }
    }
}
