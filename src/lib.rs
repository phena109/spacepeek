pub mod cli;
pub mod geometry;
pub mod keys;
pub mod preview;
pub mod selection;
pub mod state;

pub use cli::{parse_command, Command};
pub use geometry::{
    effective_dpi, place_preview, preview_placement, scaled_font_height, scaled_preview_size,
    Placement, PxRect, PxSize, LOGICAL_FONT_HEIGHT, LOGICAL_PREVIEW_HEIGHT, LOGICAL_PREVIEW_WIDTH,
};
pub use keys::{
    esc_action, should_intercept_esc, should_intercept_space, EscAction, FocusContext, KeyPhase,
    RepeatFilter, TrackedKey,
};
pub use preview::{
    format_byte_size, format_truncation_notice, load_text_preview, preview_text, CancelToken,
    DeniedKind, Generation, Preview, PreviewError, PreviewLimits, Truncation, DEFAULT_MAX_BYTES,
    MAX_LINE_CHARS,
};
pub use selection::choose_selected;
pub use state::{step, Effect, Event, PreviewState};
