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
    match ctx.focus_class.as_str() {
        "DirectUIHWND" => true,
        _ => false,
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
        assert!(should_intercept_space(&ctx("CabinetWClass", "DirectUIHWND", false, false)));
    }

    #[test]
    fn denies_edit_focus() {
        assert!(!should_intercept_space(&ctx("CabinetWClass", "Edit", false, false)));
    }

    #[test]
    fn denies_modifiers() {
        assert!(!should_intercept_space(&ctx("CabinetWClass", "DirectUIHWND", false, true)));
    }

    #[test]
    fn denies_non_explorer_foreground() {
        assert!(!should_intercept_space(&ctx("Notepad", "Edit", false, false)));
    }

    #[test]
    fn preview_visible_intercepts_when_focused_in_preview() {
        assert!(should_intercept_space(&ctx("SpacepeekPreview", "Edit", true, false)));
    }

    #[test]
    fn preview_not_visible_denies_other_foregrounds() {
        assert!(!should_intercept_space(&ctx("SpacepeekPreview", "Edit", false, false)));
    }
}
