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
}
