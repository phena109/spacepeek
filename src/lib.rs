/// Build version string.
///
/// Taken from `SPACEPEEK_BUILD_VERSION` when that environment variable is set
/// to a non-empty value at compile time, otherwise from `CARGO_PKG_VERSION`.
pub const VERSION: &str = env!("SPACEPEEK_VERSION");

pub mod chrome;
pub mod cli;
pub mod geometry;
pub mod image_preview;
pub mod keys;
pub mod preview;
pub mod selection;
pub mod state;

pub use chrome::{
    chrome_for_error, chrome_for_text, hit_test, min_physical_size, scaled_close_width,
    scaled_detail_font, scaled_header_font, scaled_header_height, scaled_header_pad, ChromeLayout,
    HeaderText, HitZone, LogicalSize, Presented, SizeMemory, WindowKind,
};
pub use cli::{parse_command, Command};
pub use geometry::{
    effective_dpi, place_preview, preview_placement, scaled_font_height, scaled_preview_size,
    Placement, PxRect, PxSize, LOGICAL_FONT_HEIGHT, LOGICAL_PREVIEW_HEIGHT, LOGICAL_PREVIEW_WIDTH,
};
pub use image_preview::{
    decode_image, fit_size, format_image_details, image_dest_rect, sniff_image, DecodedImage,
    ImageError, ImageFormat, ImageLimits,
};
pub use keys::{
    classify_typeahead_vk, decide_hook_key, esc_action, hook_key_from_vk, nav_reload_due,
    nav_reload_remaining, should_intercept_esc, should_intercept_space, typeahead_window_ms,
    wheel_scroll_dy, EscAction, EscQuery, FocusContext, HookContext, HookDecision, HookKey,
    KeyPhase, Modifiers, NavKey, RepeatFilter, ScrollHold, ScrollOp, SessionPhase, TrackedKey,
    Typeahead, TypeaheadKey, NAV_RELOAD_DELAY_MS,
};
pub use preview::{
    format_byte_size, format_compact_size, format_truncation_notice, load_text_preview,
    preview_text, CancelToken, DeniedKind, Generation, Preview, PreviewError, PreviewLimits,
    Truncation, DEFAULT_MAX_BYTES, MAX_LINE_UTF16,
};
pub use selection::{choose_selected, nonempty_path};
pub use state::{
    gate_show, session_phase, step, Effect, Event, PreviewState, ShowGate, ShowOutcome,
};
