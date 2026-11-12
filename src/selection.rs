/// Choose the item a preview should open.
///
/// `focused` is used only when `focused_is_selected` is true. Otherwise the
/// first selected item is used. An empty selection (`first_selected` is
/// `None`, and the focus is not selected) yields `None`.
pub fn choose_selected<T>(
    focused: Option<T>,
    focused_is_selected: bool,
    first_selected: Option<T>,
) -> Option<T> {
    match focused {
        Some(focused) if focused_is_selected => Some(focused),
        _ => first_selected,
    }
}

/// Drop an empty path. Nothing selected, and a shell item with no filesystem
/// path, are both `None`. Space must not open a window for either.
pub fn nonempty_path(path: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    path.filter(|path| !path.as_os_str().is_empty())
}

/// `EVENT_OBJECT_SELECTION`. The hook listens through
/// [`SHELL_EVENT_SELECTION_WITHIN`] so one click's burst shares one reload.
pub const SHELL_EVENT_SELECTION: u32 = 0x8006;
/// `EVENT_OBJECT_SELECTIONADD`.
pub const SHELL_EVENT_SELECTION_ADD: u32 = 0x8007;
/// `EVENT_OBJECT_SELECTIONREMOVE`.
pub const SHELL_EVENT_SELECTION_REMOVE: u32 = 0x8008;
/// `EVENT_OBJECT_SELECTIONWITHIN`.
pub const SHELL_EVENT_SELECTION_WITHIN: u32 = 0x8009;
/// `WM_LBUTTONUP`. The low-level mouse hook reports client button messages.
pub const MOUSE_LEFT_BUTTON_UP: u32 = 0x0202;
/// `WM_RBUTTONUP`. A right-click also selects the file under the pointer.
pub const MOUSE_RIGHT_BUTTON_UP: u32 = 0x0205;

/// Where a shell event or a pointer release landed, relative to this session.
///
/// The Windows side classifies the window. [`SelectionTarget::Preview`] is
/// checked before the origin, so a click on the preview (header, ×, or body)
/// stays `Preview` when that window overlaps Explorer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionTarget {
    /// The origin Explorer window or one of its descendants.
    Origin,
    /// The preview window or one of its children.
    Preview,
    /// Some other window.
    Elsewhere,
}

/// Whether a shell WinEvent should schedule a selection reload.
///
/// True only asks the UI thread to re-read Explorer. The existing navigation
/// delay collapses a burst, and an empty or unchanged path is ignored later.
/// Focus and location events are not selection changes.
pub fn should_reload_after_shell_event(
    session_active: bool,
    event: u32,
    target: SelectionTarget,
) -> bool {
    session_active && target == SelectionTarget::Origin && is_shell_selection_event(event)
}

/// Left or right button release. Presses, moves, and the wheel are not a
/// finished click, so they do not schedule a reload.
pub fn is_selection_pointer_message(message: u32) -> bool {
    message == MOUSE_LEFT_BUTTON_UP || message == MOUSE_RIGHT_BUTTON_UP
}

/// Whether a low-level mouse message should schedule a selection reload.
///
/// The release is not the new path. Explorer applies the selection on the
/// click, and the caller waits the existing navigation delay before reading
/// it. A release on the preview does not schedule a reload.
pub fn should_reload_after_pointer(
    session_active: bool,
    message: u32,
    target: SelectionTarget,
) -> bool {
    session_active && target == SelectionTarget::Origin && is_selection_pointer_message(message)
}

fn is_shell_selection_event(event: u32) -> bool {
    (SHELL_EVENT_SELECTION..=SHELL_EVENT_SELECTION_WITHIN).contains(&event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focused_item_wins_when_it_is_selected() {
        assert_eq!(choose_selected(Some(1), true, Some(2)), Some(1));
        assert_eq!(choose_selected(Some(1), true, None), Some(1));
    }

    #[test]
    fn unselected_focus_falls_back_to_the_first_selected_item() {
        assert_eq!(choose_selected(Some(1), false, Some(2)), Some(2));
    }

    #[test]
    fn empty_selection_is_none_even_if_an_item_is_focused() {
        assert_eq!(choose_selected(Some(1), false, None::<i32>), None);
    }

    #[test]
    fn missing_focus_uses_the_first_selected_item() {
        assert_eq!(choose_selected(None::<i32>, false, Some(2)), Some(2));
        assert_eq!(choose_selected(None::<i32>, true, Some(2)), Some(2));
    }

    #[test]
    fn nothing_selected_and_nothing_focused_is_none() {
        assert_eq!(choose_selected(None::<i32>, false, None::<i32>), None);
        assert_eq!(choose_selected(None::<i32>, true, None::<i32>), None);
    }

    #[test]
    fn nonempty_path_rejects_a_missing_selection() {
        use std::path::PathBuf;
        assert_eq!(nonempty_path(None), None);
        assert_eq!(nonempty_path(Some(PathBuf::new())), None);
        assert_eq!(
            nonempty_path(Some(PathBuf::from("ノート.md"))),
            Some(PathBuf::from("ノート.md"))
        );
        assert_eq!(
            nonempty_path(Some(PathBuf::from("notes.md"))),
            Some(PathBuf::from("notes.md"))
        );
    }

    #[test]
    fn follow_signals_use_the_win32_selection_and_button_up_codes() {
        assert_eq!(SHELL_EVENT_SELECTION, 0x8006);
        assert_eq!(SHELL_EVENT_SELECTION_ADD, 0x8007);
        assert_eq!(SHELL_EVENT_SELECTION_REMOVE, 0x8008);
        assert_eq!(SHELL_EVENT_SELECTION_WITHIN, 0x8009);
        assert_eq!(MOUSE_LEFT_BUTTON_UP, 0x0202);
        assert_eq!(MOUSE_RIGHT_BUTTON_UP, 0x0205);
    }

    #[test]
    fn shell_selection_on_the_origin_schedules_a_reload_while_a_session_is_open() {
        for event in [
            SHELL_EVENT_SELECTION,
            SHELL_EVENT_SELECTION_ADD,
            SHELL_EVENT_SELECTION_REMOVE,
            SHELL_EVENT_SELECTION_WITHIN,
        ] {
            assert!(should_reload_after_shell_event(
                true,
                event,
                SelectionTarget::Origin
            ));
            assert!(!should_reload_after_shell_event(
                false,
                event,
                SelectionTarget::Origin
            ));
        }
    }

    #[test]
    fn other_shell_events_and_windows_do_not_schedule_a_reload() {
        // Focus, reorder, and state changes are not a file-list selection.
        for event in [0x8004_u32, 0x8005, 0x800A, 0] {
            assert!(!should_reload_after_shell_event(
                true,
                event,
                SelectionTarget::Origin
            ));
        }
        assert!(!should_reload_after_shell_event(
            true,
            SHELL_EVENT_SELECTION,
            SelectionTarget::Preview
        ));
        assert!(!should_reload_after_shell_event(
            true,
            SHELL_EVENT_SELECTION,
            SelectionTarget::Elsewhere
        ));
    }

    #[test]
    fn pointer_release_on_the_origin_schedules_a_reload() {
        for message in [MOUSE_LEFT_BUTTON_UP, MOUSE_RIGHT_BUTTON_UP] {
            assert!(is_selection_pointer_message(message));
            assert!(should_reload_after_pointer(
                true,
                message,
                SelectionTarget::Origin
            ));
            assert!(!should_reload_after_pointer(
                false,
                message,
                SelectionTarget::Origin
            ));
        }
    }

    #[test]
    fn preview_clicks_presses_and_other_windows_do_not_schedule_a_reload() {
        let left_down = 0x0201_u32;
        let move_msg = 0x0200_u32;
        let wheel = 0x020A_u32;
        let double_click = 0x0203_u32;
        assert!(!is_selection_pointer_message(left_down));
        assert!(!is_selection_pointer_message(move_msg));
        assert!(!is_selection_pointer_message(wheel));
        assert!(!is_selection_pointer_message(double_click));
        for message in [
            MOUSE_LEFT_BUTTON_UP,
            MOUSE_RIGHT_BUTTON_UP,
            left_down,
            move_msg,
        ] {
            assert!(!should_reload_after_pointer(
                true,
                message,
                SelectionTarget::Preview
            ));
            assert!(!should_reload_after_pointer(
                true,
                message,
                SelectionTarget::Elsewhere
            ));
        }
    }
}
