//! Shell-icon size, layout, and fetch policy for unsupported files.
//!
//! Win32 glue lives in `main`. This module is the part that can be tested on
//! Linux: which outcomes request an icon, which pixel size a DPI asks for,
//! where that icon sits relative to the existing message text, and whether a
//! posted result is still for the live preview.

use std::path::Path;

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
///
/// Pixels with alpha 0 also have zero RGB. `AlphaBlend` adds source colour
/// even when alpha is 0, so leftover RGB would darken the preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellIconPixels {
    pub width: i32,
    pub height: i32,
    pub bgra: Vec<u8>,
}

impl ShellIconPixels {
    /// `None` when the side is not positive, the buffer is short, or the
    /// side is above [`SHELL_ICON_MAX_PX`].
    ///
    /// Same as [`Self::from_bgra_and_mask`] with no AND mask. A colour
    /// buffer whose alpha is 0 everywhere is then treated as opaque.
    pub fn from_bgra(width: i32, height: i32, bgra: Vec<u8>) -> Option<Self> {
        Self::from_bgra_and_mask(width, height, bgra, None)
    }

    /// Build pixels from a 32-bit colour buffer and an optional 1-bit AND mask.
    ///
    /// `mask` is top-down, DWORD-aligned, most-significant bit first. A set
    /// bit is transparent. It is used only when every colour pixel has alpha
    /// 0 (a legacy bitmap with no alpha channel). A missing or short mask
    /// then makes the bitmap opaque. Other buffers are premultiplied when a
    /// non-zero alpha pixel has colour above that alpha. Alpha 0 never
    /// counts as that signal, and its RGB is cleared.
    pub fn from_bgra_and_mask(
        width: i32,
        height: i32,
        mut bgra: Vec<u8>,
        mask: Option<&[u8]>,
    ) -> Option<Self> {
        if !shell_icon_pixel_size_ok(width, height) {
            return None;
        }
        let expected = pixel_byte_len(width, height)?;
        if bgra.len() < expected {
            return None;
        }
        if color_alpha_absent(&bgra[..expected]) {
            let used_mask = mask.is_some_and(|mask| {
                apply_icon_and_mask(&mut bgra[..expected], width, height, mask)
            });
            if !used_mask {
                set_opaque_alpha(&mut bgra[..expected]);
            }
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
        let Some(nbytes) = pixel_byte_len(self.width, self.height) else {
            return;
        };
        let nbytes = nbytes.min(self.bgra.len());
        if needs_premultiply(&self.bgra[..nbytes]) {
            premultiply_bgra_in_place(&mut self.bgra[..nbytes]);
        }
        clear_transparent_rgb(&mut self.bgra[..nbytes]);
    }
}

/// True when `width` and `height` are a shell-icon bitmap we will keep.
pub fn shell_icon_pixel_size_ok(width: i32, height: i32) -> bool {
    width > 0 && height > 0 && width <= SHELL_ICON_MAX_PX && height <= SHELL_ICON_MAX_PX
}

fn pixel_byte_len(width: i32, height: i32) -> Option<usize> {
    usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)
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
        Some(
            PreviewError::Binary { .. }
                | PreviewError::Denied { .. }
                | PreviewError::Svg { .. }
                | PreviewError::Pdf { .. },
        )
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

/// True when cached pixels belong to the file that is about to be shown.
///
/// A same-path reload must not drop the bitmap. A different path must.
pub fn should_keep_shell_icon_pixels(cached_path: Option<&Path>, next_path: &Path) -> bool {
    cached_path.is_some_and(|cached| cached == next_path)
}

/// True when a worker should still be started for `wanted`.
///
/// False when there is no request, that generation already timed out, a
/// lookup for the same path and size already completed, or a worker is
/// already fetching this generation and size. Path and size match even
/// when the generation changed, so a same-path reload keeps the icon.
pub fn should_fetch_shell_icon(
    wanted: Option<(Generation, &Path, i32)>,
    pixels: Option<(Generation, &Path, i32)>,
    inflight: Option<(Generation, i32)>,
    given_up: Option<Generation>,
) -> bool {
    let Some((gen, path, size_px)) = wanted else {
        return false;
    };
    if !should_retry_icon_request(gen, given_up) {
        return false;
    }
    if pixels
        .is_some_and(|(_, cached_path, cached_size)| cached_path == path && cached_size == size_px)
    {
        return false;
    }
    if inflight.is_some_and(|(inflight_gen, inflight_size)| {
        inflight_gen == gen && inflight_size == size_px
    }) {
        return false;
    }
    true
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

/// True unless this generation is currently wanted at a different size.
///
/// A DPI change keeps the generation and replaces the wanted side length.
/// The bitmap fetched for the old side is then stale and must be dropped.
/// No wanted size for this generation is not a mismatch. The caller still
/// starts a fetch for the size it wants now.
pub fn shell_icon_size_still_wanted(
    result_gen: Generation,
    result_size_px: i32,
    wanted: Option<(Generation, i32)>,
) -> bool {
    match wanted {
        Some((gen, size_px)) if gen == result_gen => result_size_px == size_px,
        _ => true,
    }
}

/// True when a pixel with non-zero alpha has a colour channel above that
/// alpha, which a premultiplied buffer cannot. Alpha 0 is ignored: shell
/// bitmaps often leave leftover RGB in pixels that are already transparent.
pub fn needs_premultiply(bgra: &[u8]) -> bool {
    bgra.chunks_exact(4).any(|px| {
        let alpha = px[3];
        alpha != 0 && (px[0] > alpha || px[1] > alpha || px[2] > alpha)
    })
}

/// DWORD-aligned byte width of one row of a 1-bit icon AND mask.
pub fn icon_mask_stride(width: i32) -> Option<usize> {
    let width = usize::try_from(width).ok()?;
    if width == 0 {
        return None;
    }
    width.checked_add(31)?.checked_div(32)?.checked_mul(4)
}

/// True when every pixel's alpha is 0. An empty or partial trailing byte
/// does not count.
fn color_alpha_absent(bgra: &[u8]) -> bool {
    !bgra.is_empty() && bgra.len() % 4 == 0 && bgra.chunks_exact(4).all(|px| px[3] == 0)
}

fn set_opaque_alpha(bgra: &mut [u8]) {
    for px in bgra.chunks_exact_mut(4) {
        px[3] = 255;
    }
}

/// Derive alpha from a 1-bit icon AND mask. A set bit is transparent
/// (alpha 0); a clear bit is opaque (alpha 255). Rows are top-down and
/// DWORD-aligned, and the leftmost pixel is the high bit of each byte.
/// Returns false, without writing, when `mask` does not cover the bitmap.
pub fn apply_icon_and_mask(bgra: &mut [u8], width: i32, height: i32, mask: &[u8]) -> bool {
    let Some(stride) = icon_mask_stride(width) else {
        return false;
    };
    let Ok(width_px) = usize::try_from(width) else {
        return false;
    };
    let Ok(height_px) = usize::try_from(height) else {
        return false;
    };
    if height_px == 0 {
        return false;
    }
    let Some(mask_bytes) = stride.checked_mul(height_px) else {
        return false;
    };
    let Some(color_bytes) = width_px
        .checked_mul(height_px)
        .and_then(|n| n.checked_mul(4))
    else {
        return false;
    };
    if mask.len() < mask_bytes || bgra.len() < color_bytes {
        return false;
    }
    for y in 0..height_px {
        let row = &mask[y * stride..y * stride + stride];
        for x in 0..width_px {
            let alpha = if mask_bit_is_set(row, x) { 0 } else { 255 };
            bgra[(y * width_px + x) * 4 + 3] = alpha;
        }
    }
    true
}

fn mask_bit_is_set(row: &[u8], x: usize) -> bool {
    let shift = 7 - (x % 8);
    row[x / 8] & (1 << shift) != 0
}

/// Zero RGB where alpha is 0 so a later `AlphaBlend` cannot add it.
fn clear_transparent_rgb(bgra: &mut [u8]) {
    for px in bgra.chunks_exact_mut(4) {
        if px[3] == 0 {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
        }
    }
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
    use crate::geometry::PxRect;
    use crate::preview::{DeniedKind, PreviewError};
    use std::path::Path;

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
        assert!(should_request_shell_icon(Some(&PreviewError::svg(
            crate::svg::SvgError::Malformed
        ))));
        assert!(should_request_shell_icon(Some(&PreviewError::pdf(
            crate::pdf::PdfError::Rendering
        ))));
        assert!(should_request_shell_icon(Some(&PreviewError::pdf(
            crate::pdf::PdfError::PasswordProtected
        ))));
        assert!(!should_request_shell_icon(Some(&PreviewError::Image(
            crate::image_preview::ImageError::Corrupt
        ))));
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

    #[test]
    fn transparent_leftover_does_not_premultiply_again() {
        // Alpha 0 with leftover RGB is normal in a premultiplied shell bitmap.
        assert!(!needs_premultiply(&[255, 128, 64, 0]));
        assert!(!needs_premultiply(&[255, 128, 64, 0, 80, 40, 20, 160]));
        // A real straight pixel still counts, even beside a transparent one.
        assert!(needs_premultiply(&[255, 0, 0, 128, 255, 255, 255, 0]));

        let bgra = vec![80, 40, 20, 160, 255, 128, 64, 0];
        let pixels = ShellIconPixels::from_bgra(1, 2, bgra).expect("pixels");
        assert_eq!(&pixels.bgra[0..4], &[80, 40, 20, 160]);
        assert_eq!(&pixels.bgra[4..8], &[0, 0, 0, 0]);
        let again = ShellIconPixels::from_bgra(1, 2, pixels.bgra.clone()).expect("stable");
        assert_eq!(again.bgra, pixels.bgra);
    }

    #[test]
    fn straight_alpha_premultiplies_and_clears_transparent_rgb() {
        let bgra = vec![255, 128, 0, 128, 9, 8, 7, 0];
        let pixels = ShellIconPixels::from_bgra(1, 2, bgra).expect("pixels");
        assert_eq!(pixels.bgra[0], mul_255(255, 128));
        assert_eq!(pixels.bgra[1], mul_255(128, 128));
        assert_eq!(pixels.bgra[2], 0);
        assert_eq!(pixels.bgra[3], 128);
        assert_eq!(&pixels.bgra[4..8], &[0, 0, 0, 0]);
    }

    #[test]
    fn missing_alpha_without_mask_is_opaque() {
        let pixels =
            ShellIconPixels::from_bgra(1, 2, vec![10, 20, 30, 0, 40, 50, 60, 0]).expect("pixels");
        assert_eq!(pixels.bgra, vec![10, 20, 30, 255, 40, 50, 60, 255]);
        // One real alpha sample keeps the buffer on the alpha path.
        let mixed =
            ShellIconPixels::from_bgra(1, 2, vec![10, 20, 30, 0, 1, 2, 3, 4]).expect("mixed");
        assert_eq!(&mixed.bgra[0..4], &[0, 0, 0, 0]);
        assert_eq!(&mixed.bgra[4..8], &[1, 2, 3, 4]);
    }

    #[test]
    fn missing_alpha_derives_opacity_from_and_mask() {
        assert_eq!(icon_mask_stride(1), Some(4));
        assert_eq!(icon_mask_stride(32), Some(4));
        assert_eq!(icon_mask_stride(33), Some(8));
        assert_eq!(icon_mask_stride(0), None);

        // 2×1. Bit 7 is pixel 0 (clear = opaque), bit 6 is pixel 1 (set = transparent).
        let mask = [0x40, 0, 0, 0];
        let pixels = ShellIconPixels::from_bgra_and_mask(
            2,
            1,
            vec![10, 20, 30, 0, 40, 50, 60, 0],
            Some(&mask),
        )
        .expect("pixels");
        assert_eq!(&pixels.bgra[0..4], &[10, 20, 30, 255]);
        assert_eq!(&pixels.bgra[4..8], &[0, 0, 0, 0]);

        // 1×2, stride 4. Top bit set, bottom bit clear. Padding must not shift row 1.
        let rows = [
            0x80, 0, 0, 0, // row 0
            0x00, 0, 0, 0, // row 1
        ];
        let pixels = ShellIconPixels::from_bgra_and_mask(
            1,
            2,
            vec![10, 20, 30, 0, 40, 50, 60, 0],
            Some(&rows),
        )
        .expect("rows");
        assert_eq!(&pixels.bgra[0..4], &[0, 0, 0, 0]);
        assert_eq!(&pixels.bgra[4..8], &[40, 50, 60, 255]);

        // Width 8: only the low bit of the first byte (pixel 7) is transparent.
        let mut wide = vec![0u8; 8 * 4];
        for px in wide.chunks_exact_mut(4) {
            px[0] = 9;
        }
        let mut wide_mask = [0u8; 4];
        wide_mask[0] = 0x01;
        let pixels =
            ShellIconPixels::from_bgra_and_mask(8, 1, wide, Some(&wide_mask)).expect("wide");
        for (i, px) in pixels.bgra.chunks_exact(4).enumerate() {
            if i == 7 {
                assert_eq!(px, &[0, 0, 0, 0]);
            } else {
                assert_eq!(px, &[9, 0, 0, 255]);
            }
        }

        // A real alpha channel wins over the mask.
        let kept = ShellIconPixels::from_bgra_and_mask(
            1,
            1,
            vec![10, 20, 30, 128],
            Some(&[0x80, 0, 0, 0]),
        )
        .expect("kept");
        assert_eq!(kept.bgra, vec![10, 20, 30, 128]);

        // A short mask is not applied; the legacy bitmap stays opaque.
        let fallback =
            ShellIconPixels::from_bgra_and_mask(1, 1, vec![10, 20, 30, 0], Some(&[0x80]))
                .expect("fallback");
        assert_eq!(fallback.bgra, vec![10, 20, 30, 255]);

        let mut untouched = vec![10, 20, 30, 0];
        assert!(!apply_icon_and_mask(&mut untouched, 1, 1, &[0x80]));
        assert_eq!(untouched, vec![10, 20, 30, 0]);
    }

    #[test]
    fn dpi_size_change_drops_the_older_icon() {
        let (token, gen) = token_and_gen();
        let live = Some(gen);
        assert!(accept_shell_icon_result(
            gen,
            live,
            &token,
            10,
            SHELL_ICON_TIMEOUT_MS
        ));
        assert!(shell_icon_size_still_wanted(gen, 96, Some((gen, 96))));
        assert!(!shell_icon_size_still_wanted(gen, 96, Some((gen, 144))));
        let show_stale = shell_icon_size_still_wanted(gen, 96, Some((gen, 144)))
            && accept_shell_icon_result(gen, live, &token, 10, SHELL_ICON_TIMEOUT_MS);
        assert!(!show_stale);
        let show_current = shell_icon_size_still_wanted(gen, 144, Some((gen, 144)))
            && accept_shell_icon_result(gen, live, &token, 10, SHELL_ICON_TIMEOUT_MS);
        assert!(show_current);
        // Another generation, or no request, is not a size mismatch.
        let other = Generation::new(gen.value + 1);
        assert!(shell_icon_size_still_wanted(gen, 96, Some((other, 144))));
        assert!(shell_icon_size_still_wanted(gen, 96, None));
    }

    #[test]
    fn matching_cached_icon_is_not_fetched_again() {
        let gen = Generation::new(1);
        let path = Path::new("notes.pdf");
        let wanted = Some((gen, path, 96));
        let pixels = Some((gen, path, 96));
        // Watch timer: wanted stays set, inflight is cleared after success.
        assert!(!should_fetch_shell_icon(wanted, pixels, None, None));
        assert!(!should_fetch_shell_icon(
            wanted,
            pixels,
            Some((gen, 96)),
            None
        ));
    }

    #[test]
    fn first_icon_request_fetches() {
        let gen = Generation::new(1);
        let path = Path::new("notes.pdf");
        assert!(should_fetch_shell_icon(
            Some((gen, path, 96)),
            None,
            None,
            None
        ));
        assert!(!should_fetch_shell_icon(None, None, None, None));
        assert!(!should_fetch_shell_icon(
            None,
            Some((gen, path, 96)),
            None,
            None
        ));
    }

    #[test]
    fn same_path_new_generation_keeps_cached_icon() {
        let path = Path::new("notes.pdf");
        assert!(!should_fetch_shell_icon(
            Some((Generation::new(2), path, 96)),
            Some((Generation::new(1), path, 96)),
            None,
            None
        ));
        assert!(should_keep_shell_icon_pixels(Some(path), path));
        assert!(!should_keep_shell_icon_pixels(
            Some(path),
            Path::new("other.pdf")
        ));
        assert!(!should_keep_shell_icon_pixels(None, path));
    }

    #[test]
    fn dpi_size_change_fetches_again() {
        let gen = Generation::new(1);
        let path = Path::new("notes.pdf");
        assert!(should_fetch_shell_icon(
            Some((gen, path, 144)),
            Some((gen, path, 96)),
            None,
            None
        ));
    }

    #[test]
    fn different_path_fetches() {
        assert!(should_fetch_shell_icon(
            Some((Generation::new(2), Path::new("other.pdf"), 96)),
            Some((Generation::new(1), Path::new("notes.pdf"), 96)),
            None,
            None
        ));
        assert!(!should_keep_shell_icon_pixels(
            Some(Path::new("notes.pdf")),
            Path::new("other.pdf")
        ));
    }

    #[test]
    fn inflight_or_given_up_does_not_fetch() {
        let gen = Generation::new(1);
        let path = Path::new("notes.pdf");
        assert!(!should_fetch_shell_icon(
            Some((gen, path, 96)),
            None,
            Some((gen, 96)),
            None
        ));
        assert!(should_fetch_shell_icon(
            Some((gen, path, 144)),
            None,
            Some((gen, 96)),
            None
        ));
        assert!(!should_fetch_shell_icon(
            Some((gen, path, 96)),
            None,
            None,
            Some(gen)
        ));
        assert!(should_fetch_shell_icon(
            Some((Generation::new(2), path, 96)),
            None,
            None,
            Some(gen)
        ));
    }

    #[test]
    fn completed_failed_lookup_is_not_retried() {
        let gen = Generation::new(1);
        let path = Path::new("notes.pdf");
        // Identity of a finished lookup, even when no bitmap was stored.
        assert!(!should_fetch_shell_icon(
            Some((gen, path, 96)),
            Some((gen, path, 96)),
            None,
            None
        ));
    }
}
