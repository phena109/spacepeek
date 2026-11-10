#[derive(Debug, Clone)]
pub struct FocusContext {
    pub foreground_class: String,
    pub focus_class: String,
    pub preview_visible: bool,
    pub modifiers_down: bool,
}

pub fn should_intercept_space(ctx: &FocusContext) -> bool {
    if ctx.modifiers_down {
        return false;
    }
    if ctx.preview_visible && ctx.foreground_class == "SpacepeekPreview" {
        return true;
    }
    if ctx.foreground_class != "CabinetWClass" {
        return false;
    }
    matches!(ctx.focus_class.as_str(), "DirectUIHWND")
}

/// What the low-level hook should do with an Esc key-down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscAction {
    /// Do not post, and do not swallow.
    Ignore,
    /// Post Esc so the in-flight load can cancel, and let Explorer see the key.
    CancelLoad,
    /// Post Esc and swallow it. The preview window is in front.
    Dismiss,
}

/// Esc dismisses the preview when that window is foreground.
/// Esc cancels a load when the originating Explorer window is foreground and
/// the preview window does not exist yet. Modifiers always pass the key through.
/// A foreground preview wins over a load still marked in progress.
pub fn esc_action(
    preview_is_foreground: bool,
    loading_in_origin_explorer: bool,
    modifiers_down: bool,
) -> EscAction {
    if modifiers_down {
        EscAction::Ignore
    } else if preview_is_foreground {
        EscAction::Dismiss
    } else if loading_in_origin_explorer {
        EscAction::CancelLoad
    } else {
        EscAction::Ignore
    }
}

/// True when Esc should be swallowed. Cancel-during-load is [`esc_action`]:
/// that posts and does not swallow.
pub fn should_intercept_esc(preview_is_foreground: bool, modifiers_down: bool) -> bool {
    matches!(
        esc_action(preview_is_foreground, false, modifiers_down),
        EscAction::Dismiss
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackedKey {
    Space,
    Esc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatDecision {
    /// Post the key to the UI thread. True only for the first down that is intercepted.
    pub act: bool,
    /// Drop the event. Repeats and the matching key-up stay swallowed when the first down was.
    pub swallow: bool,
}

#[derive(Debug, Clone, Copy)]
struct KeyLatch {
    down: bool,
    swallowed: bool,
}

impl KeyLatch {
    const fn new() -> Self {
        Self {
            down: false,
            swallowed: false,
        }
    }
}

/// Auto-repeat filter for Space and Esc.
///
/// Low-level hooks deliver auto-repeat as extra key-downs. The first down may
/// act. Later downs do not act, but they are still swallowed when that first
/// down was swallowed, so the focused app never sees a partial press.
///
/// Key-up always clears the latch (a missed up is the only way it can stick,
/// and every delivered up clears it). The key-up is swallowed when its down
/// was swallowed, so the app does not observe a key-up without a key-down.
/// A key-up whose down was passed through is passed through too.
#[derive(Debug, Clone)]
pub struct RepeatFilter {
    space: KeyLatch,
    esc: KeyLatch,
}

impl RepeatFilter {
    pub const fn new() -> Self {
        Self {
            space: KeyLatch::new(),
            esc: KeyLatch::new(),
        }
    }

    /// `intercept` is the hook policy for this key-down. It is ignored for
    /// repeats and for key-up; those follow the latch from the first down.
    pub fn observe(&mut self, key: TrackedKey, phase: KeyPhase, intercept: bool) -> RepeatDecision {
        let latch = self.latch_mut(key);
        match phase {
            KeyPhase::Down => {
                if latch.down {
                    RepeatDecision {
                        act: false,
                        swallow: latch.swallowed,
                    }
                } else {
                    latch.down = true;
                    latch.swallowed = intercept;
                    RepeatDecision {
                        act: intercept,
                        swallow: intercept,
                    }
                }
            }
            KeyPhase::Up => {
                let swallow = latch.down && latch.swallowed;
                *latch = KeyLatch::new();
                RepeatDecision {
                    act: false,
                    swallow,
                }
            }
        }
    }

    /// The first down was not actually taken (the UI post failed). Repeats and
    /// key-up then pass through. No effect once the latch has been cleared.
    pub fn set_swallowed(&mut self, key: TrackedKey, swallowed: bool) {
        let latch = self.latch_mut(key);
        if latch.down {
            latch.swallowed = swallowed;
        }
    }

    fn latch_mut(&mut self, key: TrackedKey) -> &mut KeyLatch {
        match key {
            TrackedKey::Space => &mut self.space,
            TrackedKey::Esc => &mut self.esc,
        }
    }
}

impl Default for RepeatFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(foreground: &str, focus: &str, visible: bool, modifiers: bool) -> FocusContext {
        FocusContext {
            foreground_class: foreground.to_string(),
            focus_class: focus.to_string(),
            preview_visible: visible,
            modifiers_down: modifiers,
        }
    }

    #[test]
    fn allows_direct_ui_in_explorer() {
        assert!(should_intercept_space(&ctx(
            "CabinetWClass",
            "DirectUIHWND",
            false,
            false
        )));
    }

    #[test]
    fn denies_edit_focus() {
        assert!(!should_intercept_space(&ctx(
            "CabinetWClass",
            "Edit",
            false,
            false
        )));
    }

    #[test]
    fn denies_modifiers() {
        assert!(!should_intercept_space(&ctx(
            "CabinetWClass",
            "DirectUIHWND",
            false,
            true
        )));
    }

    #[test]
    fn denies_non_explorer_foreground() {
        assert!(!should_intercept_space(&ctx(
            "Notepad", "Edit", false, false
        )));
    }

    #[test]
    fn preview_visible_intercepts_when_focused_in_preview() {
        assert!(should_intercept_space(&ctx(
            "SpacepeekPreview",
            "Edit",
            true,
            false
        )));
    }

    #[test]
    fn preview_not_visible_denies_other_foregrounds() {
        assert!(!should_intercept_space(&ctx(
            "SpacepeekPreview",
            "Edit",
            false,
            false
        )));
    }

    #[test]
    fn esc_action_matrix() {
        assert_eq!(esc_action(false, false, false), EscAction::Ignore);
        assert_eq!(esc_action(false, false, true), EscAction::Ignore);
        assert_eq!(esc_action(false, true, false), EscAction::CancelLoad);
        assert_eq!(esc_action(false, true, true), EscAction::Ignore);
        assert_eq!(esc_action(true, false, false), EscAction::Dismiss);
        assert_eq!(esc_action(true, false, true), EscAction::Ignore);
        // A visible preview is dismissed even if a load flag is still set.
        assert_eq!(esc_action(true, true, false), EscAction::Dismiss);
        assert_eq!(esc_action(true, true, true), EscAction::Ignore);
    }

    #[test]
    fn should_intercept_esc_delegates_to_dismiss() {
        assert!(should_intercept_esc(true, false));
        assert!(!should_intercept_esc(true, true));
        assert!(!should_intercept_esc(false, false));
        assert!(!should_intercept_esc(false, true));
        assert!(matches!(esc_action(true, false, false), EscAction::Dismiss));
        assert!(!matches!(
            esc_action(false, true, false),
            EscAction::Dismiss
        ));
    }

    #[test]
    fn repeat_filter_first_down_acts_repeats_swallowed_up_resets() {
        let mut filter = RepeatFilter::new();
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Down, true),
            RepeatDecision {
                act: true,
                swallow: true
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Down, true),
            RepeatDecision {
                act: false,
                swallow: true
            }
        );
        // A repeat keeps the original swallow decision even if the policy flipped.
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Down, false),
            RepeatDecision {
                act: false,
                swallow: true
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Up, true),
            RepeatDecision {
                act: false,
                swallow: true
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Down, true),
            RepeatDecision {
                act: true,
                swallow: true
            }
        );
    }

    #[test]
    fn repeat_filter_passes_through_when_first_down_is_not_intercepted() {
        let mut filter = RepeatFilter::new();
        assert_eq!(
            filter.observe(TrackedKey::Esc, KeyPhase::Down, false),
            RepeatDecision {
                act: false,
                swallow: false
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Esc, KeyPhase::Down, true),
            RepeatDecision {
                act: false,
                swallow: false
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Esc, KeyPhase::Up, true),
            RepeatDecision {
                act: false,
                swallow: false
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Esc, KeyPhase::Down, true),
            RepeatDecision {
                act: true,
                swallow: true
            }
        );
    }

    #[test]
    fn repeat_filter_keys_are_independent_and_up_without_down_does_not_stick() {
        let mut filter = RepeatFilter::new();
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Up, true),
            RepeatDecision {
                act: false,
                swallow: false
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Down, true),
            RepeatDecision {
                act: true,
                swallow: true
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Esc, KeyPhase::Down, true),
            RepeatDecision {
                act: true,
                swallow: true
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Down, true),
            RepeatDecision {
                act: false,
                swallow: true
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Esc, KeyPhase::Up, false),
            RepeatDecision {
                act: false,
                swallow: true
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Up, false),
            RepeatDecision {
                act: false,
                swallow: true
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Down, false),
            RepeatDecision {
                act: false,
                swallow: false
            }
        );
    }

    #[test]
    fn repeat_filter_failed_swallow_lets_the_rest_of_the_press_pass() {
        let mut filter = RepeatFilter::new();
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Down, true),
            RepeatDecision {
                act: true,
                swallow: true
            }
        );
        filter.set_swallowed(TrackedKey::Space, false);
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Down, true),
            RepeatDecision {
                act: false,
                swallow: false
            }
        );
        assert_eq!(
            filter.observe(TrackedKey::Space, KeyPhase::Up, false),
            RepeatDecision {
                act: false,
                swallow: false
            }
        );
    }
}
