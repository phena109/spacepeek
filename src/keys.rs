//! Keyboard policy for the low-level hook.
//!
//! The hook thread only collects booleans and a tick. [`decide_hook_key`]
//! turns that into an action. Timing helpers take the tick as an input so
//! wraparound can be tested without a clock.

#[derive(Debug, Clone)]
pub struct FocusContext {
    pub foreground_class: String,
    pub focus_class: String,
    /// A preview session is active (loading, pending show, or on screen).
    pub preview_visible: bool,
    pub modifiers_down: bool,
    /// Explorer type-ahead still owns the next Space.
    pub typeahead_active: bool,
}

/// How visible the preview session is to the keyboard hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SessionPhase {
    Hidden = 0,
    /// Load in flight, or content ready, and the window is not on screen.
    Pending = 1,
    /// The preview window is on screen, including while a new file reloads.
    Visible = 2,
}

impl SessionPhase {
    pub const fn from_code(code: u8) -> Self {
        match code {
            1 => SessionPhase::Pending,
            2 => SessionPhase::Visible,
            _ => SessionPhase::Hidden,
        }
    }

    pub const fn is_hidden(self) -> bool {
        matches!(self, SessionPhase::Hidden)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub win: bool,
    pub shift: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        ctrl: false,
        alt: false,
        win: false,
        shift: false,
    };

    /// Shift is included. Space and Esc with any of these modifiers pass through.
    pub const fn blocks_preview_toggle(self) -> bool {
        self.ctrl || self.alt || self.win || self.shift
    }

    /// Ctrl, Alt, or the Windows key. Shift still produces a typed character.
    pub const fn command(self) -> bool {
        self.ctrl || self.alt || self.win
    }
}

impl Default for Modifiers {
    fn default() -> Self {
        Self::NONE
    }
}

/// Everything the hook knows when a key goes down, without Win32 types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookContext {
    pub phase: SessionPhase,
    pub explorer_list_focused: bool,
    pub preview_is_foreground: bool,
    pub origin_is_foreground: bool,
    pub modifiers: Modifiers,
    pub typeahead_active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKey {
    Space,
    Esc,
    Nav(NavKey),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavKey {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollOp {
    PageUp,
    PageDown,
    Top,
    Bottom,
}

impl ScrollOp {
    pub const fn code(self) -> u8 {
        match self {
            ScrollOp::PageUp => 1,
            ScrollOp::PageDown => 2,
            ScrollOp::Top => 3,
            ScrollOp::Bottom => 4,
        }
    }

    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(ScrollOp::PageUp),
            2 => Some(ScrollOp::PageDown),
            3 => Some(ScrollOp::Top),
            4 => Some(ScrollOp::Bottom),
            _ => None,
        }
    }
}

/// What the low-level hook should do with one key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookDecision {
    /// Deliver the key unchanged.
    Pass,
    /// Open or close the preview. Post Space and swallow the key.
    Space,
    /// Close a visible preview. Post Esc and swallow the key.
    Dismiss,
    /// Cancel a preview that is not on screen yet. Post Esc and do not swallow,
    /// so Explorer still sees the key.
    CancelPending,
    /// Explorer keeps the key (it moves the selection). The UI re-reads the
    /// selection after it has been applied.
    FollowSelection,
    /// Swallow the key and scroll the preview text.
    Scroll(ScrollOp),
}

/// Key to action while a preview is hidden, pending, or visible.
///
/// Arrow keys are not remapped per view. They are delivered to Explorer, and
/// the preview reloads only when the selected path changes. In Details view,
/// Left and Right often leave the file in place.
pub fn decide_hook_key(ctx: &HookContext, key: HookKey) -> HookDecision {
    match key {
        HookKey::Space => decide_space(ctx),
        HookKey::Esc => decide_esc(ctx),
        HookKey::Nav(nav) => decide_nav(ctx, nav),
    }
}

fn decide_space(ctx: &HookContext) -> HookDecision {
    if ctx.modifiers.blocks_preview_toggle() {
        return HookDecision::Pass;
    }
    if ctx.phase != SessionPhase::Hidden && ctx.preview_is_foreground {
        return HookDecision::Space;
    }
    if ctx.typeahead_active && ctx.explorer_list_focused {
        return HookDecision::Pass;
    }
    if ctx.explorer_list_focused {
        HookDecision::Space
    } else {
        HookDecision::Pass
    }
}

fn decide_esc(ctx: &HookContext) -> HookDecision {
    if ctx.modifiers.blocks_preview_toggle() {
        return HookDecision::Pass;
    }
    match ctx.phase {
        SessionPhase::Visible if ctx.origin_is_foreground || ctx.preview_is_foreground => {
            HookDecision::Dismiss
        }
        SessionPhase::Pending if ctx.origin_is_foreground => HookDecision::CancelPending,
        _ => HookDecision::Pass,
    }
}

fn decide_nav(ctx: &HookContext, key: NavKey) -> HookDecision {
    if ctx.modifiers.alt || ctx.modifiers.win {
        return HookDecision::Pass;
    }
    let in_session = ctx.explorer_list_focused || ctx.preview_is_foreground;
    if !in_session {
        return HookDecision::Pass;
    }
    match key {
        NavKey::PageUp | NavKey::PageDown | NavKey::Home | NavKey::End
            if ctx.phase == SessionPhase::Visible =>
        {
            // Ctrl+PageUp and Ctrl+PageDown switch Explorer tabs.
            if ctx.modifiers.ctrl && matches!(key, NavKey::PageUp | NavKey::PageDown) {
                return HookDecision::Pass;
            }
            match scroll_op(key) {
                Some(op) => HookDecision::Scroll(op),
                None => HookDecision::Pass,
            }
        }
        NavKey::Left | NavKey::Right | NavKey::Up | NavKey::Down
            if ctx.phase != SessionPhase::Hidden
                && ctx.explorer_list_focused
                && !ctx.modifiers.ctrl =>
        {
            HookDecision::FollowSelection
        }
        _ => HookDecision::Pass,
    }
}

fn scroll_op(key: NavKey) -> Option<ScrollOp> {
    match key {
        NavKey::PageUp => Some(ScrollOp::PageUp),
        NavKey::PageDown => Some(ScrollOp::PageDown),
        NavKey::Home => Some(ScrollOp::Top),
        NavKey::End => Some(ScrollOp::Bottom),
        NavKey::Left | NavKey::Right | NavKey::Up | NavKey::Down => None,
    }
}

pub fn should_intercept_space(ctx: &FocusContext) -> bool {
    matches!(
        decide_hook_key(&hook_context_from_focus(ctx), HookKey::Space),
        HookDecision::Space
    )
}

fn hook_context_from_focus(ctx: &FocusContext) -> HookContext {
    let explorer_list_focused =
        ctx.foreground_class == "CabinetWClass" && ctx.focus_class == "DirectUIHWND";
    let preview_is_foreground = ctx.foreground_class == "SpacepeekPreview";
    HookContext {
        // Space does not care whether the window is pending or already visible.
        phase: if ctx.preview_visible {
            SessionPhase::Visible
        } else {
            SessionPhase::Hidden
        },
        explorer_list_focused,
        preview_is_foreground,
        origin_is_foreground: explorer_list_focused,
        modifiers: if ctx.modifiers_down {
            Modifiers {
                ctrl: true,
                alt: true,
                win: true,
                shift: true,
            }
        } else {
            Modifiers::NONE
        },
        typeahead_active: ctx.typeahead_active,
    }
}

/// What the low-level hook should do with an Esc key-down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscAction {
    /// Do not post, and do not swallow.
    Ignore,
    /// Post Esc so the in-flight load can cancel, and let Explorer see the key.
    CancelLoad,
    /// Post Esc and swallow it. The preview is on screen.
    Dismiss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EscQuery {
    pub phase: SessionPhase,
    pub origin_is_foreground: bool,
    pub preview_is_foreground: bool,
    pub modifiers_down: bool,
}

/// Esc dismisses a visible preview when the origin Explorer (or the preview
/// itself) is in front. Esc cancels a not-yet-visible preview when that
/// Explorer is in front. Modifiers always pass the key through.
pub fn esc_action(query: EscQuery) -> EscAction {
    let ctx = HookContext {
        phase: query.phase,
        explorer_list_focused: query.origin_is_foreground,
        preview_is_foreground: query.preview_is_foreground,
        origin_is_foreground: query.origin_is_foreground,
        modifiers: if query.modifiers_down {
            Modifiers {
                ctrl: true,
                alt: false,
                win: false,
                shift: false,
            }
        } else {
            Modifiers::NONE
        },
        typeahead_active: false,
    };
    match decide_hook_key(&ctx, HookKey::Esc) {
        HookDecision::Dismiss => EscAction::Dismiss,
        HookDecision::CancelPending => EscAction::CancelLoad,
        _ => EscAction::Ignore,
    }
}

/// True when Esc should be swallowed. Cancel-during-load is [`esc_action`]:
/// that posts and does not swallow.
pub fn should_intercept_esc(query: EscQuery) -> bool {
    matches!(esc_action(query), EscAction::Dismiss)
}

/// Virtual-key codes the hook classifies itself. Win32 headers are not
/// available to the platform-independent crate.
const VK_BACK: u16 = 0x08;
const VK_TAB: u16 = 0x09;
const VK_RETURN: u16 = 0x0D;
const VK_ESCAPE: u16 = 0x1B;
const VK_SPACE: u16 = 0x20;
const VK_PRIOR: u16 = 0x21;
const VK_NEXT: u16 = 0x22;
const VK_END: u16 = 0x23;
const VK_HOME: u16 = 0x24;
const VK_LEFT: u16 = 0x25;
const VK_UP: u16 = 0x26;
const VK_RIGHT: u16 = 0x27;
const VK_DOWN: u16 = 0x28;
const VK_DELETE: u16 = 0x2E;
const VK_0: u16 = 0x30;
const VK_9: u16 = 0x39;
const VK_A: u16 = 0x41;
const VK_Z: u16 = 0x5A;
const VK_NUMPAD0: u16 = 0x60;
const VK_DIVIDE: u16 = 0x6F;
const VK_F1: u16 = 0x70;
const VK_F24: u16 = 0x87;
const VK_OEM_1: u16 = 0xBA;
const VK_OEM_3: u16 = 0xC0;
const VK_OEM_4: u16 = 0xDB;
const VK_OEM_8: u16 = 0xDF;
const VK_OEM_102: u16 = 0xE2;

pub fn hook_key_from_vk(vk: u16) -> Option<HookKey> {
    match vk {
        VK_SPACE => Some(HookKey::Space),
        VK_ESCAPE => Some(HookKey::Esc),
        VK_PRIOR => Some(HookKey::Nav(NavKey::PageUp)),
        VK_NEXT => Some(HookKey::Nav(NavKey::PageDown)),
        VK_END => Some(HookKey::Nav(NavKey::End)),
        VK_HOME => Some(HookKey::Nav(NavKey::Home)),
        VK_LEFT => Some(HookKey::Nav(NavKey::Left)),
        VK_UP => Some(HookKey::Nav(NavKey::Up)),
        VK_RIGHT => Some(HookKey::Nav(NavKey::Right)),
        VK_DOWN => Some(HookKey::Nav(NavKey::Down)),
        _ => None,
    }
}

/// How a key that reached the Explorer list affects the type-ahead timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeaheadKey {
    /// Letter, digit, or punctuation. Arms or refreshes the timer.
    PlainChar,
    /// Backspace. Explorer edits the search string, so the timer refreshes.
    SearchEdit,
    /// A Space that was passed through because type-ahead was already active.
    PassedSpace,
    /// Navigation that ends Explorer's search.
    Cancels,
    /// Modifiers, function chords, and anything else. Does not arm or cancel.
    Ignored,
}

/// `GetDoubleClickTime() * 2`, kept inside a one-to-two second window.
/// The usual double-click time is 500 ms, which lands on one second.
pub const TYPEAHEAD_WINDOW_FLOOR_MS: u32 = 1000;
pub const TYPEAHEAD_WINDOW_CAP_MS: u32 = 2000;

pub fn typeahead_window_ms(double_click_ms: u32) -> u32 {
    double_click_ms
        .saturating_mul(2)
        .clamp(TYPEAHEAD_WINDOW_FLOOR_MS, TYPEAHEAD_WINDOW_CAP_MS)
}

/// Milliseconds from `earlier` to `later` on a wrapping 32-bit tick.
pub fn elapsed_ms(earlier: u32, later: u32) -> u32 {
    later.wrapping_sub(earlier)
}

pub fn within_ms(earlier: u32, later: u32, window_ms: u32) -> bool {
    elapsed_ms(earlier, later) < window_ms
}

pub fn classify_typeahead_vk(vk: u16, modifiers: Modifiers) -> TypeaheadKey {
    if modifiers.command() {
        return TypeaheadKey::Ignored;
    }
    if is_plain_char_vk(vk) {
        TypeaheadKey::PlainChar
    } else if vk == VK_BACK {
        TypeaheadKey::SearchEdit
    } else if cancels_typeahead_vk(vk) {
        TypeaheadKey::Cancels
    } else {
        TypeaheadKey::Ignored
    }
}

fn is_plain_char_vk(vk: u16) -> bool {
    (VK_0..=VK_9).contains(&vk)
        || (VK_A..=VK_Z).contains(&vk)
        || (VK_NUMPAD0..=VK_DIVIDE).contains(&vk)
        || (VK_OEM_1..=VK_OEM_3).contains(&vk)
        || (VK_OEM_4..=VK_OEM_8).contains(&vk)
        || vk == VK_OEM_102
}

fn cancels_typeahead_vk(vk: u16) -> bool {
    matches!(
        vk,
        VK_TAB
            | VK_RETURN
            | VK_ESCAPE
            | VK_PRIOR
            | VK_NEXT
            | VK_END
            | VK_HOME
            | VK_LEFT
            | VK_UP
            | VK_RIGHT
            | VK_DOWN
            | VK_DELETE
    ) || (VK_F1..=VK_F24).contains(&vk)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Typeahead {
    last_plain_tick: Option<u32>,
    window_ms: u32,
}

impl Typeahead {
    pub const fn new(window_ms: u32) -> Self {
        Self {
            last_plain_tick: None,
            window_ms,
        }
    }

    pub fn set_window_ms(&mut self, window_ms: u32) {
        self.window_ms = window_ms;
    }

    pub fn clear(&mut self) {
        self.last_plain_tick = None;
    }

    pub fn observe(&mut self, key: TypeaheadKey, tick: u32) {
        match key {
            TypeaheadKey::PlainChar | TypeaheadKey::SearchEdit | TypeaheadKey::PassedSpace => {
                self.last_plain_tick = Some(tick);
            }
            TypeaheadKey::Cancels => self.last_plain_tick = None,
            TypeaheadKey::Ignored => {}
        }
    }

    pub fn active(&self, tick: u32) -> bool {
        self.last_plain_tick
            .is_some_and(|last| within_ms(last, tick, self.window_ms))
    }
}

impl Default for Typeahead {
    fn default() -> Self {
        Self::new(TYPEAHEAD_WINDOW_FLOOR_MS)
    }
}

/// Coalesce arrow auto-repeat before re-reading Explorer's selection.
/// Also gives Explorer time to apply the key. This is a delay, not a measurement.
pub const NAV_RELOAD_DELAY_MS: u32 = 150;

pub fn nav_reload_due(last_nav_tick: Option<u32>, now: u32, delay_ms: u32) -> bool {
    last_nav_tick.is_some_and(|tick| elapsed_ms(tick, now) >= delay_ms)
}

/// Time left until a navigation reload. `None` when nothing is armed.
/// `Some(0)` means the reload is due.
pub fn nav_reload_remaining(last_nav_tick: Option<u32>, now: u32, delay_ms: u32) -> Option<u32> {
    let tick = last_nav_tick?;
    let elapsed = elapsed_ms(tick, now);
    if elapsed >= delay_ms {
        Some(0)
    } else {
        Some(delay_ms - elapsed)
    }
}

/// Positive `wheel_delta` is away from the user and scrolls toward the start.
/// The result is the vertical line count for `EM_LINESCROLL` (positive scrolls down).
pub fn wheel_scroll_dy(wheel_delta: i32, lines_per_notch: i32) -> i32 {
    if lines_per_notch == 0 || wheel_delta == 0 {
        return 0;
    }
    let notches = wheel_delta / 120;
    notches.saturating_mul(lines_per_notch).saturating_neg()
}

/// Page, Home, and End repeats must keep scrolling, and the matching key-up
/// stays swallowed so Explorer does not see an up without a down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollHold {
    held: u8,
}

impl ScrollHold {
    pub const fn new() -> Self {
        Self { held: 0 }
    }

    pub fn observe(&mut self, key: NavKey, phase: KeyPhase, take: bool) -> RepeatDecision {
        let Some(bit) = scroll_bit(key) else {
            return RepeatDecision {
                act: false,
                swallow: false,
            };
        };
        match phase {
            KeyPhase::Down => {
                if take {
                    self.held |= bit;
                    RepeatDecision {
                        act: true,
                        swallow: true,
                    }
                } else if self.held & bit != 0 {
                    // The preview closed mid-gesture. Keep swallowing the rest
                    // of the press, but do not scroll a window that is gone.
                    RepeatDecision {
                        act: false,
                        swallow: true,
                    }
                } else {
                    RepeatDecision {
                        act: false,
                        swallow: false,
                    }
                }
            }
            KeyPhase::Up => {
                let owned = self.held & bit != 0;
                self.held &= !bit;
                RepeatDecision {
                    act: false,
                    swallow: owned,
                }
            }
        }
    }

    pub fn release(&mut self, key: NavKey) {
        if let Some(bit) = scroll_bit(key) {
            self.held &= !bit;
        }
    }

    pub fn clear(&mut self) {
        self.held = 0;
    }
}

impl Default for ScrollHold {
    fn default() -> Self {
        Self::new()
    }
}

fn scroll_bit(key: NavKey) -> Option<u8> {
    match key {
        NavKey::PageUp => Some(1 << 0),
        NavKey::PageDown => Some(1 << 1),
        NavKey::Home => Some(1 << 2),
        NavKey::End => Some(1 << 3),
        NavKey::Left | NavKey::Right | NavKey::Up | NavKey::Down => None,
    }
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
            typeahead_active: false,
        }
    }

    fn list_ctx(phase: SessionPhase) -> HookContext {
        HookContext {
            phase,
            explorer_list_focused: true,
            preview_is_foreground: false,
            origin_is_foreground: true,
            modifiers: Modifiers::NONE,
            typeahead_active: false,
        }
    }

    fn esc_query(
        phase: SessionPhase,
        origin_is_foreground: bool,
        preview_is_foreground: bool,
        modifiers_down: bool,
    ) -> EscQuery {
        EscQuery {
            phase,
            origin_is_foreground,
            preview_is_foreground,
            modifiers_down,
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
    fn typeahead_space_passes_through_to_the_list() {
        let mut ctx = list_ctx(SessionPhase::Hidden);
        ctx.typeahead_active = true;
        assert_eq!(decide_hook_key(&ctx, HookKey::Space), HookDecision::Pass);
        ctx.phase = SessionPhase::Visible;
        assert_eq!(decide_hook_key(&ctx, HookKey::Space), HookDecision::Pass);
    }

    #[test]
    fn space_without_typeahead_toggles_from_the_list() {
        assert_eq!(
            decide_hook_key(&list_ctx(SessionPhase::Hidden), HookKey::Space),
            HookDecision::Space
        );
        assert_eq!(
            decide_hook_key(&list_ctx(SessionPhase::Pending), HookKey::Space),
            HookDecision::Space
        );
        assert_eq!(
            decide_hook_key(&list_ctx(SessionPhase::Visible), HookKey::Space),
            HookDecision::Space
        );
    }

    #[test]
    fn esc_action_matrix() {
        assert_eq!(
            esc_action(esc_query(SessionPhase::Hidden, false, false, false)),
            EscAction::Ignore
        );
        assert_eq!(
            esc_action(esc_query(SessionPhase::Hidden, false, false, true)),
            EscAction::Ignore
        );
        assert_eq!(
            esc_action(esc_query(SessionPhase::Pending, true, false, false)),
            EscAction::CancelLoad
        );
        assert_eq!(
            esc_action(esc_query(SessionPhase::Pending, true, false, true)),
            EscAction::Ignore
        );
        assert_eq!(
            esc_action(esc_query(SessionPhase::Pending, false, false, false)),
            EscAction::Ignore
        );
        assert_eq!(
            esc_action(esc_query(SessionPhase::Visible, true, false, false)),
            EscAction::Dismiss
        );
        assert_eq!(
            esc_action(esc_query(SessionPhase::Visible, false, true, false)),
            EscAction::Dismiss
        );
        assert_eq!(
            esc_action(esc_query(SessionPhase::Visible, true, false, true)),
            EscAction::Ignore
        );
        // A visible preview is dismissed even if a load is still marked pending.
        assert_eq!(
            esc_action(esc_query(SessionPhase::Visible, true, true, false)),
            EscAction::Dismiss
        );
        assert_eq!(
            esc_action(esc_query(SessionPhase::Visible, false, false, false)),
            EscAction::Ignore
        );
    }

    #[test]
    fn should_intercept_esc_delegates_to_dismiss() {
        let dismiss = esc_query(SessionPhase::Visible, true, false, false);
        let cancel = esc_query(SessionPhase::Pending, true, false, false);
        assert!(should_intercept_esc(dismiss));
        assert!(!should_intercept_esc(esc_query(
            SessionPhase::Visible,
            true,
            false,
            true
        )));
        assert!(!should_intercept_esc(cancel));
        assert!(!should_intercept_esc(esc_query(
            SessionPhase::Hidden,
            false,
            false,
            false
        )));
        assert!(matches!(esc_action(dismiss), EscAction::Dismiss));
        assert!(!matches!(esc_action(cancel), EscAction::Dismiss));
    }

    #[test]
    fn arrows_follow_selection_only_while_a_session_is_open() {
        for phase in [SessionPhase::Pending, SessionPhase::Visible] {
            let ctx = list_ctx(phase);
            for key in [NavKey::Up, NavKey::Down, NavKey::Left, NavKey::Right] {
                assert_eq!(
                    decide_hook_key(&ctx, HookKey::Nav(key)),
                    HookDecision::FollowSelection,
                    "{phase:?} {key:?}"
                );
            }
        }
        let hidden = list_ctx(SessionPhase::Hidden);
        assert_eq!(
            decide_hook_key(&hidden, HookKey::Nav(NavKey::Down)),
            HookDecision::Pass
        );
    }

    #[test]
    fn page_home_end_scroll_only_while_visible() {
        let visible = list_ctx(SessionPhase::Visible);
        assert_eq!(
            decide_hook_key(&visible, HookKey::Nav(NavKey::PageUp)),
            HookDecision::Scroll(ScrollOp::PageUp)
        );
        assert_eq!(
            decide_hook_key(&visible, HookKey::Nav(NavKey::PageDown)),
            HookDecision::Scroll(ScrollOp::PageDown)
        );
        assert_eq!(
            decide_hook_key(&visible, HookKey::Nav(NavKey::Home)),
            HookDecision::Scroll(ScrollOp::Top)
        );
        assert_eq!(
            decide_hook_key(&visible, HookKey::Nav(NavKey::End)),
            HookDecision::Scroll(ScrollOp::Bottom)
        );
        let mut ctrl_end = visible;
        ctrl_end.modifiers.ctrl = true;
        assert_eq!(
            decide_hook_key(&ctrl_end, HookKey::Nav(NavKey::End)),
            HookDecision::Scroll(ScrollOp::Bottom)
        );
        assert_eq!(
            decide_hook_key(&ctrl_end, HookKey::Nav(NavKey::PageDown)),
            HookDecision::Pass
        );

        for phase in [SessionPhase::Hidden, SessionPhase::Pending] {
            let ctx = list_ctx(phase);
            assert_eq!(
                decide_hook_key(&ctx, HookKey::Nav(NavKey::PageDown)),
                HookDecision::Pass,
                "{phase:?}"
            );
            assert_eq!(
                decide_hook_key(&ctx, HookKey::Nav(NavKey::Home)),
                HookDecision::Pass,
                "{phase:?}"
            );
        }
    }

    #[test]
    fn command_modifiers_do_not_steal_navigation() {
        let mut ctx = list_ctx(SessionPhase::Visible);
        ctx.modifiers.ctrl = true;
        assert_eq!(
            decide_hook_key(&ctx, HookKey::Nav(NavKey::Down)),
            HookDecision::Pass
        );
        ctx.modifiers = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            decide_hook_key(&ctx, HookKey::Nav(NavKey::PageDown)),
            HookDecision::Pass
        );
        ctx.modifiers = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        assert_eq!(
            decide_hook_key(&ctx, HookKey::Nav(NavKey::Down)),
            HookDecision::FollowSelection
        );
    }

    #[test]
    fn typeahead_window_is_double_click_time_clamped() {
        assert_eq!(typeahead_window_ms(500), 1000);
        assert_eq!(typeahead_window_ms(0), 1000);
        assert_eq!(typeahead_window_ms(800), 1600);
        assert_eq!(typeahead_window_ms(5_000), 2000);
        assert_eq!(typeahead_window_ms(u32::MAX), 2000);
    }

    #[test]
    fn typeahead_inside_outside_and_wraparound() {
        let mut t = Typeahead::new(1000);
        t.observe(TypeaheadKey::PlainChar, 0);
        assert!(t.active(0));
        assert!(t.active(999));
        assert!(!t.active(1000));

        t.observe(TypeaheadKey::PlainChar, u32::MAX - 50);
        // 51 ticks to wrap, plus 100.
        assert!(t.active(100));
        assert!(!t.active(5_000));

        t.observe(TypeaheadKey::Cancels, 5_000);
        assert!(!t.active(5_010));
    }

    #[test]
    fn typeahead_ignores_command_modifiers_and_shift_types() {
        let plain = classify_typeahead_vk(
            b'A' as u16,
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
        );
        assert_eq!(plain, TypeaheadKey::PlainChar);
        for modifiers in [
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
            Modifiers {
                alt: true,
                ..Modifiers::NONE
            },
            Modifiers {
                win: true,
                ..Modifiers::NONE
            },
        ] {
            assert_eq!(
                classify_typeahead_vk(b'A' as u16, modifiers),
                TypeaheadKey::Ignored,
                "{modifiers:?}"
            );
        }
        let mut t = Typeahead::new(1000);
        t.observe(
            classify_typeahead_vk(
                b'A' as u16,
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                },
            ),
            0,
        );
        assert!(!t.active(10));
        t.observe(plain, 20);
        assert!(t.active(30));
        t.observe(TypeaheadKey::PassedSpace, 40);
        assert!(t.active(50));
        assert!(!t.active(1040));
    }

    #[test]
    fn typeahead_classifies_punctuation_backspace_and_navigation() {
        assert_eq!(
            classify_typeahead_vk(VK_OEM_1, Modifiers::NONE),
            TypeaheadKey::PlainChar
        );
        assert_eq!(
            classify_typeahead_vk(VK_NUMPAD0, Modifiers::NONE),
            TypeaheadKey::PlainChar
        );
        assert_eq!(
            classify_typeahead_vk(VK_BACK, Modifiers::NONE),
            TypeaheadKey::SearchEdit
        );
        assert_eq!(
            classify_typeahead_vk(VK_DOWN, Modifiers::NONE),
            TypeaheadKey::Cancels
        );
        assert_eq!(
            classify_typeahead_vk(VK_SPACE, Modifiers::NONE),
            TypeaheadKey::Ignored
        );
        assert_eq!(
            classify_typeahead_vk(VK_F1, Modifiers::NONE),
            TypeaheadKey::Cancels
        );
        let mut t = Typeahead::new(1000);
        t.observe(TypeaheadKey::SearchEdit, 10);
        assert!(t.active(20));
    }

    #[test]
    fn passed_space_refreshes_the_typeahead_window() {
        let mut t = Typeahead::new(1000);
        t.observe(TypeaheadKey::PlainChar, 0);
        assert!(t.active(800));
        t.observe(TypeaheadKey::PassedSpace, 800);
        assert!(t.active(1_700));
        assert!(!t.active(1_800));
    }

    #[test]
    fn nav_reload_due_waits_and_wraps() {
        assert!(!nav_reload_due(None, 1_000, 150));
        assert!(!nav_reload_due(Some(1_000), 1_149, 150));
        assert!(nav_reload_due(Some(1_000), 1_150, 150));
        assert_eq!(nav_reload_remaining(None, 0, 150), None);
        assert_eq!(nav_reload_remaining(Some(1_000), 1_100, 150), Some(50));
        assert_eq!(nav_reload_remaining(Some(1_000), 1_200, 150), Some(0));
        let earlier = u32::MAX - 10;
        assert!(!nav_reload_due(Some(earlier), 20, 150));
        assert!(nav_reload_due(Some(earlier), 200, 150));
        assert_eq!(nav_reload_remaining(Some(earlier), 20, 150), Some(119));
    }

    #[test]
    fn wheel_delta_becomes_line_scroll() {
        assert_eq!(wheel_scroll_dy(120, 3), -3);
        assert_eq!(wheel_scroll_dy(-240, 3), 6);
        assert_eq!(wheel_scroll_dy(60, 3), 0);
        assert_eq!(wheel_scroll_dy(120, 0), 0);
    }

    #[test]
    fn scroll_codes_round_trip() {
        for op in [
            ScrollOp::PageUp,
            ScrollOp::PageDown,
            ScrollOp::Top,
            ScrollOp::Bottom,
        ] {
            assert_eq!(ScrollOp::from_code(op.code()), Some(op));
        }
        assert_eq!(ScrollOp::from_code(0), None);
    }

    #[test]
    fn scroll_hold_repeats_act_and_up_is_swallowed() {
        let mut hold = ScrollHold::new();
        assert_eq!(
            hold.observe(NavKey::PageDown, KeyPhase::Down, true),
            RepeatDecision {
                act: true,
                swallow: true
            }
        );
        assert_eq!(
            hold.observe(NavKey::PageDown, KeyPhase::Down, true),
            RepeatDecision {
                act: true,
                swallow: true
            }
        );
        assert_eq!(
            hold.observe(NavKey::PageDown, KeyPhase::Up, false),
            RepeatDecision {
                act: false,
                swallow: true
            }
        );
        assert_eq!(
            hold.observe(NavKey::PageDown, KeyPhase::Down, false),
            RepeatDecision {
                act: false,
                swallow: false
            }
        );
        assert_eq!(
            hold.observe(NavKey::Down, KeyPhase::Down, true),
            RepeatDecision {
                act: false,
                swallow: false
            }
        );
    }

    #[test]
    fn scroll_hold_release_lets_the_up_pass() {
        let mut hold = ScrollHold::new();
        let _ = hold.observe(NavKey::Home, KeyPhase::Down, true);
        hold.release(NavKey::Home);
        assert_eq!(
            hold.observe(NavKey::Home, KeyPhase::Up, false),
            RepeatDecision {
                act: false,
                swallow: false
            }
        );
    }

    #[test]
    fn hook_key_from_vk_maps_space_esc_and_nav() {
        assert_eq!(hook_key_from_vk(VK_SPACE), Some(HookKey::Space));
        assert_eq!(hook_key_from_vk(VK_ESCAPE), Some(HookKey::Esc));
        assert_eq!(hook_key_from_vk(VK_DOWN), Some(HookKey::Nav(NavKey::Down)));
        assert_eq!(hook_key_from_vk(b'A' as u16), None);
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
