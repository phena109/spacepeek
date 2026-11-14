//! Shell-icon size, layout, and fetch policy for unsupported files.
//!
//! Win32 glue lives in `main`. This module is the part that can be tested on
//! Linux: which outcomes request an icon, which pixel size a DPI asks for,
//! where that icon sits relative to the existing message text, and whether a
//! posted result is still for the live preview.

use crate::geometry::{effective_dpi, scale_for_dpi, PxRect};
use crate::preview::{CancelToken, Generation, PreviewError};

/// Logical icon side at 96 DPI. Physical pixels are `MulDiv(96, dpi, 96)`.
pub const SHELL_ICON_LOGICAL_PX: i32 = 96;
/// Logical gap between the icon and the message text at 96 DPI.
pub const SHELL_ICON_GAP_LOGICAL: i32 = 12;
/// Hide the icon rather than draw it smaller than this, in logical pixels.
pub const SHELL_ICON_MIN_LOGICAL: i32 = 16;
/// Largest bitmap side the worker may keep. Jumbo image lists are 256.
pub const SHELL_ICON_MAX_PX: i32 = 256;
/// A hung shell lookup is abandoned after this many milliseconds.
pub const SHELL_ICON_TIMEOUT_MS: u64 = 1_000;
/// At most one in-flight fetch plus one abandoned hung worker.
pub const SHELL_ICON_WORKER_CAP: u32 = 2;

/// `SHIL_LARGE` (32px).
pub const SHELL_IL_LARGE: i32 = 0;
/// `SHIL_EXTRALARGE` (48px).
pub const SHELL_IL_EXTRALARGE: i32 = 2;
/// `SHIL_JUMBO` (256px).
pub const SHELL_IL_JUMBO: i32 = 4;

/// Premultiplied BGRA, top-down, 4 bytes per pixel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIconPixels {
    pub width: i32,
    pub height: i32,
    pub bgra: Vec<u8>,
}

impl ShellIconPixels {
    /// `None` when the side is not positive, the buffer is short, or the
    /// side is above [`SHELL_ICON_MAX_PX`].
    pub fn from_bgra(width: i32, height: i32, bgra: Vec<u8>) -> Option<Self> {
        if !shell_icon_pixel_size_ok(width, height) {
            return None;
        }
        let expected = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?
            .checked_mul(4)?;
        if bgra.len() < expected {
            return None;
        }
        let mut pixels = Self {
            width,
            height,
            bgra,
        };
        pixels.ensure_premultiplied();
        Some(pixels)
    }

    fn ensure_premultiplied(&mut self) {
        if needs_premultiply(&self.bgra) {
            premultiply_bgra_in_place(&mut self.bgra);
        }
    }
}

/// True when `width` and `height` are a shell-icon bitmap we will keep.
pub fn shell_icon_pixel_size_ok(width: i32, height: i32) -> bool {
    width > 0 && height > 0 && width <= SHELL_ICON_MAX_PX && height <= SHELL_ICON_MAX_PX
}

/// Icon side in physical pixels for `dpi`. A missing DPI (`0`) is 96.
pub fn shell_icon_px(dpi: u32) -> i32 {
    scale_for_dpi(SHELL_ICON_LOGICAL_PX, effective_dpi(dpi)).max(1)
}

/// Gap between icon and text, in physical pixels, for `dpi`.
pub fn shell_icon_gap_px(dpi: u32) -> i32 {
    scale_for_dpi(SHELL_ICON_GAP_LOGICAL, effective_dpi(dpi)).max(0)
}

/// Smallest icon side that is still drawn, in physical pixels, for `dpi`.
pub fn shell_icon_min_px(dpi: u32) -> i32 {
    scale_for_dpi(SHELL_ICON_MIN_LOGICAL, effective_dpi(dpi)).max(1)
}

/// True only for the unsupported / "doesn't look like text" outcomes that
/// already show type, size, and modified lines.
///
/// Text, image, folder, empty, shortcut, and access-denied paths stay false
/// even if those errors later grow a [`crate::preview::QuickInfo`] payload.
pub fn should_request_shell_icon(error: Option<&PreviewError>) -> bool {
    matches!(
        error,
        Some(PreviewError::Binary { .. } | PreviewError::Denied { .. })
    )
}

/// `SHGetImageList` size id that covers `icon_px` without asking for a
/// thumbnail. Jumbo (256) above 48, extra-large (48) above 32, else large (32).
pub fn shell_image_list_id(icon_px: i32) -> i32 {
    if icon_px > 48 {
        SHELL_IL_JUMBO
    } else if icon_px > 32 {
        SHELL_IL_EXTRALARGE
    } else {
        SHELL_IL_LARGE
    }
}

/// Where the icon and the existing centred message sit in the body.
///
/// The text rectangle is the vertical centre of `body` using `text_height`,
/// matching the message painter. The icon uses leftover space above that
/// text, so a late bitmap cannot move the words. When leftover is tight the
/// icon shrinks, then hides, rather than overlapping the text. A failed
/// lookup leaves this same layout with an empty icon slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellIconLayout {
    pub icon: Option<PxRect>,
    pub text: PxRect,
}

/// Layout the reserved icon slot above already-centred message text.
pub fn layout_shell_icon_message(
    body: PxRect,
    text_height: i32,
    icon_px: i32,
    gap: i32,
    min_icon_px: i32,
) -> ShellIconLayout {
    let text = centered_text_rect(body, text_height);
    let icon = icon_rect_above_text(body, text, icon_px, gap, min_icon_px);
    ShellIconLayout { icon, text }
}

fn centered_text_rect(body: PxRect, text_height: i32) -> PxRect {
    let body_h = body.height();
    let text_h = text_height.max(0).min(body_h);
    let top = if text_h >= body_h {
        body.top
    } else {
        body.top.saturating_add((body_h - text_h) / 2)
    };
    PxRect {
        left: body.left,
        top,
        right: body.right,
        bottom: top.saturating_add(text_h),
    }
}

fn icon_rect_above_text(
    body: PxRect,
    text: PxRect,
    icon_px: i32,
    gap: i32,
    min_icon_px: i32,
) -> Option<PxRect> {
    if icon_px <= 0 || min_icon_px <= 0 {
        return None;
    }
    let gap = gap.max(0);
    let above = text.top.saturating_sub(body.top);
    let budget_h = above.saturating_sub(gap);
    let budget_w = body.width();
    if budget_h < min_icon_px || budget_w < min_icon_px {
        return None;
    }
    let size = icon_px.min(budget_h).min(budget_w);
    if size < min_icon_px {
        return None;
    }
    let left = body.left.saturating_add((budget_w - size) / 2);
    let bottom = text.top.saturating_sub(gap);
    let top = bottom.saturating_sub(size);
    if top < body.top {
        return None;
    }
    Some(PxRect {
        left,
        top,
        right: left.saturating_add(size),
        bottom,
    })
}

/// True when a posted icon belongs to the live unsupported preview and that
/// generation has not been cancelled.
pub fn accept_shell_icon_generation(
    result_gen: Generation,
    live_gen: Option<Generation>,
    cancel: &CancelToken,
) -> bool {
    live_gen == Some(result_gen) && !cancel.is_cancelled(&result_gen)
}

/// True when the worker finished inside the timeout. Equal to the bound
/// still counts as in time.
pub fn shell_icon_arrived_in_time(elapsed_ms: u64, timeout_ms: u64) -> bool {
    elapsed_ms <= timeout_ms
}

/// True when the in-flight worker has gone past the bound and may be
/// abandoned so a later file can start its own fetch.
pub fn icon_inflight_timed_out(elapsed_ms: u64, timeout_ms: u64) -> bool {
    elapsed_ms > timeout_ms
}

/// Tick-count delta. `GetTickCount` wraparound is a wrapping subtract.
pub fn tick_elapsed_ms(started: u32, now: u32) -> u64 {
    u64::from(now.wrapping_sub(started))
}

/// Do not start another worker for a generation that already timed out.
pub fn should_retry_icon_request(wanted_gen: Generation, given_up: Option<Generation>) -> bool {
    given_up != Some(wanted_gen)
}

/// Whether a new icon worker may start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconSpawn {
    /// No worker is fetching this request and we are under the cap.
    Spawn,
    /// A worker is already fetching this generation and size.
    AlreadyInFlight,
    /// A worker is busy on another file; keep only this latest request.
    Coalesce,
    /// Live workers (including abandoned hung ones) are at the cap.
    AtCap,
}

/// Decide whether to spawn, wait, or remember the latest path.
///
/// `inflight` is `None` when nothing is tracked (including after a timeout
/// abandon). `Some(true)` means the live worker is already this request.
pub fn icon_spawn_decision(inflight: Option<bool>, live_workers: u32, cap: u32) -> IconSpawn {
    match inflight {
        Some(true) => IconSpawn::AlreadyInFlight,
        Some(false) => IconSpawn::Coalesce,
        None if live_workers < cap => IconSpawn::Spawn,
        None => IconSpawn::AtCap,
    }
}

/// Accept a posted result only when the generation is live, not cancelled,
/// and the worker finished in time.
pub fn accept_shell_icon_result(
    result_gen: Generation,
    live_gen: Option<Generation>,
    cancel: &CancelToken,
    elapsed_ms: u64,
    timeout_ms: u64,
) -> bool {
    accept_shell_icon_generation(result_gen, live_gen, cancel)
        && shell_icon_arrived_in_time(elapsed_ms, timeout_ms)
}

/// True when any pixel has a colour channel above its alpha, which a
/// premultiplied buffer cannot.
pub fn needs_premultiply(bgra: &[u8]) -> bool {
    bgra.chunks_exact(4)
        .any(|px| px[0] > px[3] || px[1] > px[3] || px[2] > px[3])
}

/// Premultiply 8-bit BGRA in place. Short trailing bytes are left alone.
pub fn premultiply_bgra_in_place(bgra: &mut [u8]) {
    for px in bgra.chunks_exact_mut(4) {
        let alpha = u16::from(px[3]);
        px[0] = mul_255(px[0], alpha);
        px[1] = mul_255(px[1], alpha);
        px[2] = mul_255(px[2], alpha);
    }
}

fn mul_255(channel: u8, alpha: u16) -> u8 {
    ((u16::from(channel) * alpha + 127) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preview::{DeniedKind, PreviewError};
    use crate::geometry::PxRect;

    fn body(width: i32, height: i32) -> PxRect {
        PxRect {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    }

    fn token_and_gen() -> (CancelToken, Generation) {
        let token = CancelToken::new();
        let gen = token.generate();
        (token, gen)
    }

    #[test]
    fn icon_px_follows_dpi_steps() {
        assert_eq!(shell_icon_px(96), 96);
        assert_eq!(shell_icon_px(0), 96);
        assert_eq!(shell_icon_px(120), 120);
        assert_eq!(shell_icon_px(144), 144);
        assert_eq!(shell_icon_px(192), 192);
        assert_eq!(shell_icon_gap_px(96), 12);
        assert_eq!(shell_icon_gap_px(144), 18);
        assert_eq!(shell_icon_min_px(96), 16);
        assert_eq!(shell_icon_min_px(192), 32);
    }

    #[test]
    fn image_list_id_picks_jumbo_above_48() {
        assert_eq!(shell_image_list_id(16), SHELL_IL_LARGE);
        assert_eq!(shell_image_list_id(32), SHELL_IL_LARGE);
        assert_eq!(shell_image_list_id(33), SHELL_IL_EXTRALARGE);
        assert_eq!(shell_image_list_id(48), SHELL_IL_EXTRALARGE);
        assert_eq!(shell_image_list_id(49), SHELL_IL_JUMBO);
        assert_eq!(shell_image_list_id(96), SHELL_IL_JUMBO);
        assert_eq!(shell_image_list_id(256), SHELL_IL_JUMBO);
    }

    #[test]
    fn only_unsupported_quick_info_requests_an_icon() {
        assert!(should_request_shell_icon(Some(&PreviewError::binary())));
        assert!(should_request_shell_icon(Some(&PreviewError::denied(
            DeniedKind::Binary
        ))));
        assert!(should_request_shell_icon(Some(&PreviewError::denied(
            DeniedKind::Image
        ))));
        assert!(!should_request_shell_icon(Some(&PreviewError::Empty)));
        assert!(!should_request_shell_icon(Some(&PreviewError::Directory {
            count: None
        })));
        assert!(!should_request_shell_icon(Some(&PreviewError::Cancelled)));
        assert!(!should_request_shell_icon(Some(&PreviewError::Shortcut {
            target: None
        })));
        assert!(!should_request_shell_icon(Some(&PreviewError::Io(
            std::io::Error::from_raw_os_error(5)
        ))));
        assert!(!should_request_shell_icon(None));
    }

    #[test]
    fn layout_reserves_icon_above_centred_text() {
        let layout = layout_shell_icon_message(body(400, 300), 80, 96, 12, 16);
        let icon = layout.icon.expect("icon fits");
        assert_eq!(icon.width(), 96);
        assert_eq!(icon.height(), 96);
        assert_eq!(icon.left, (400 - 96) / 2);
        assert_eq!(layout.text.top, (300 - 80) / 2);
        assert_eq!(layout.text.bottom, layout.text.top + 80);
        assert_eq!(icon.bottom, layout.text.top - 12);
        assert!(icon.bottom <= layout.text.top);
        assert!(icon.top >= 0);
    }

    #[test]
    fn layout_shrinks_icon_when_leftover_is_tight() {
        // Body 200×160, text 40 → leftover above = 60. Budget after gap 12 = 48.
        let layout = layout_shell_icon_message(body(200, 160), 40, 96, 12, 16);
        let icon = layout.icon.expect("shrunk icon");
        assert_eq!(icon.width(), 48);
        assert_eq!(icon.height(), 48);
        assert_eq!(icon.bottom, layout.text.top - 12);
        assert!(icon.bottom <= layout.text.top);
        assert_eq!(layout.text.height(), 40);
    }

    #[test]
    fn layout_hides_icon_in_a_tiny_window() {
        let too_short = layout_shell_icon_message(body(400, 80), 40, 96, 12, 16);
        assert!(too_short.icon.is_none());
        assert_eq!(too_short.text.top, (80 - 40) / 2);
        assert_eq!(too_short.text.height(), 40);

        let too_narrow = layout_shell_icon_message(body(12, 300), 40, 96, 12, 16);
        assert!(too_narrow.icon.is_none());

        let text_fills = layout_shell_icon_message(body(400, 80), 80, 96, 12, 16);
        assert!(text_fills.icon.is_none());
        assert_eq!(text_fills.text.top, 0);
        assert_eq!(text_fills.text.bottom, 80);
    }

    #[test]
    fn layout_hides_when_asked_size_is_zero() {
        let layout = layout_shell_icon_message(body(400, 300), 40, 0, 12, 16);
        assert!(layout.icon.is_none());
        assert_eq!(layout.text.height(), 40);
    }

    #[test]
    fn layout_does_not_overlap_text_with_zero_gap() {
        let layout = layout_shell_icon_message(body(200, 200), 40, 96, 0, 16);
        let icon = layout.icon.expect("icon");
        assert_eq!(icon.bottom, layout.text.top);
        assert!(icon.right <= 200);
        assert!(icon.left >= 0);
    }

    #[test]
    fn generation_accepts_live_and_discards_stale() {
        let (token, gen) = token_and_gen();
        assert!(accept_shell_icon_generation(gen, Some(gen), &token));
        assert!(!accept_shell_icon_generation(
            gen,
            Some(Generation::new(gen.value + 1)),
            &token
        ));
        assert!(!accept_shell_icon_generation(gen, None, &token));

        let _ = token.generate();
        assert!(token.is_cancelled(&gen));
        assert!(!accept_shell_icon_generation(gen, Some(gen), &token));
    }

    #[test]
    fn timeout_bound_is_inclusive() {
        assert!(shell_icon_arrived_in_time(0, SHELL_ICON_TIMEOUT_MS));
        assert!(shell_icon_arrived_in_time(1_000, SHELL_ICON_TIMEOUT_MS));
        assert!(!shell_icon_arrived_in_time(1_001, SHELL_ICON_TIMEOUT_MS));
        assert!(!icon_inflight_timed_out(1_000, SHELL_ICON_TIMEOUT_MS));
        assert!(icon_inflight_timed_out(1_001, SHELL_ICON_TIMEOUT_MS));
    }

    #[test]
    fn tick_elapsed_handles_wrap() {
        assert_eq!(tick_elapsed_ms(10, 20), 10);
        assert_eq!(tick_elapsed_ms(u32::MAX, 0), 1);
        assert_eq!(tick_elapsed_ms(u32::MAX - 4, 5), 10);
    }

    #[test]
    fn accept_result_requires_live_gen_and_timeout() {
        let (token, gen) = token_and_gen();
        assert!(accept_shell_icon_result(
            gen,
            Some(gen),
            &token,
            500,
            SHELL_ICON_TIMEOUT_MS
        ));
        assert!(!accept_shell_icon_result(
            gen,
            Some(gen),
            &token,
            1_001,
            SHELL_ICON_TIMEOUT_MS
        ));
        let stale = Generation::new(gen.value + 3);
        assert!(!accept_shell_icon_result(
            gen,
            Some(stale),
            &token,
            10,
            SHELL_ICON_TIMEOUT_MS
        ));
    }

    #[test]
    fn spawn_coalesces_then_caps() {
        assert_eq!(
            icon_spawn_decision(None, 0, SHELL_ICON_WORKER_CAP),
            IconSpawn::Spawn
        );
        assert_eq!(
            icon_spawn_decision(Some(true), 1, SHELL_ICON_WORKER_CAP),
            IconSpawn::AlreadyInFlight
        );
        assert_eq!(
            icon_spawn_decision(Some(false), 1, SHELL_ICON_WORKER_CAP),
            IconSpawn::Coalesce
        );
        assert_eq!(
            icon_spawn_decision(None, 1, SHELL_ICON_WORKER_CAP),
            IconSpawn::Spawn
        );
        assert_eq!(
            icon_spawn_decision(None, 2, SHELL_ICON_WORKER_CAP),
            IconSpawn::AtCap
        );
        assert!(!should_retry_icon_request(
            Generation::new(4),
            Some(Generation::new(4))
        ));
        assert!(should_retry_icon_request(
            Generation::new(5),
            Some(Generation::new(4))
        ));
        assert!(should_retry_icon_request(Generation::new(4), None));
    }

    #[test]
    fn pixels_reject_bad_sizes_and_premultiply() {
        assert!(ShellIconPixels::from_bgra(0, 16, vec![0; 64]).is_none());
        assert!(ShellIconPixels::from_bgra(16, 16, vec![0; 10]).is_none());
        assert!(ShellIconPixels::from_bgra(257, 16, vec![0; 257 * 16 * 4]).is_none());
        assert!(shell_icon_pixel_size_ok(256, 256));
        assert!(!shell_icon_pixel_size_ok(257, 1));

        let straight = vec![255, 128, 0, 128];
        assert!(needs_premultiply(&straight));
        let pixels = ShellIconPixels::from_bgra(1, 1, straight).expect("1x1");
        assert_eq!(pixels.bgra[3], 128);
        assert_eq!(pixels.bgra[0], mul_255(255, 128));
        assert_eq!(pixels.bgra[1], mul_255(128, 128));
        assert_eq!(pixels.bgra[2], 0);
        assert!(!needs_premultiply(&pixels.bgra));

        let already = vec![64, 32, 16, 128];
        assert!(!needs_premultiply(&already));
        let mut copy = already.clone();
        premultiply_bgra_in_place(&mut copy);
        assert_eq!(copy[0], mul_255(64, 128));
    }
}
