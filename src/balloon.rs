//! First-run tray balloon copy and the flag that records it.
//!
//! The Windows build shows the balloon with `NIF_INFO` on the existing tray
//! icon and stores one DWORD under `HKCU\Software\Spacepeek`. This module
//! decides the strings, the value name, and whether a stored flag means the
//! balloon was already shown. It does not call Win32.

/// Title for `NOTIFYICONDATAW::szInfoTitle`.
pub const BALLOON_TITLE: &str = "Spacepeek";

/// Body for `NOTIFYICONDATAW::szInfo`.
pub const BALLOON_BODY: &str =
    "Spacepeek is running. Select a file in Explorer and press Space. Right-click this icon to quit.";

/// `szInfoTitle` capacity in `WCHAR`, including the terminating NUL.
pub const BALLOON_TITLE_WCHAR_CAP: usize = 64;

/// `szInfo` capacity in `WCHAR`, including the terminating NUL.
pub const BALLOON_BODY_WCHAR_CAP: usize = 256;

/// `uTimeout` in milliseconds. Windows Vista and later often ignore it.
pub const BALLOON_TIMEOUT_MS: u32 = 10_000;

/// Subkey of `HKEY_CURRENT_USER`. One key, not under HKLM.
pub const FIRST_RUN_SUBKEY: &str = r"Software\Spacepeek";

/// The only value stored under [`FIRST_RUN_SUBKEY`].
pub const FIRST_RUN_VALUE_NAME: &str = "FirstRunBalloonShown";

/// DWORD that means the balloon was shown. Any other stored value shows it.
pub const FIRST_RUN_SHOWN: u32 = 1;

/// UTF-16 code units in `text`, plus one for the terminating NUL.
pub fn utf16_z_units(text: &str) -> usize {
    text.encode_utf16().count().saturating_add(1)
}

/// `text` plus a NUL fits in `cap` UTF-16 units.
pub fn fits_wchar_z(text: &str, cap: usize) -> bool {
    utf16_z_units(text) <= cap
}

/// Copy `text` into `dst` as UTF-16 with a trailing NUL.
///
/// Returns `false` without writing when `text` and its NUL do not fit.
/// On success, units after the NUL are cleared.
pub fn copy_utf16_z(dst: &mut [u16], text: &str) -> bool {
    let units = text.encode_utf16().count();
    if units.saturating_add(1) > dst.len() {
        return false;
    }
    for (slot, unit) in dst.iter_mut().zip(text.encode_utf16()) {
        *slot = unit;
    }
    dst[units] = 0;
    for slot in &mut dst[units + 1..] {
        *slot = 0;
    }
    true
}

/// A stored flag is a successful query of a 4-byte DWORD.
///
/// A missing value, a failed query, another type, or another size is `None`.
pub fn interpret_dword_flag(
    query_ok: bool,
    is_dword: bool,
    byte_len: u32,
    value: u32,
) -> Option<u32> {
    if query_ok && is_dword && byte_len == 4 {
        Some(value)
    } else {
        None
    }
}

/// Show the balloon unless the stored flag is exactly [`FIRST_RUN_SHOWN`].
pub fn should_show_first_run_balloon(stored: Option<u32>) -> bool {
    stored != Some(FIRST_RUN_SHOWN)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_z(buf: &[u16]) -> String {
        let end = buf.iter().position(|&unit| unit == 0).unwrap_or(buf.len());
        String::from_utf16(&buf[..end]).expect("balloon text is valid UTF-16")
    }

    #[test]
    fn title_and_body_match_the_first_run_hint() {
        assert_eq!(BALLOON_TITLE, "Spacepeek");
        assert_eq!(
            BALLOON_BODY,
            "Spacepeek is running. Select a file in Explorer and press Space. Right-click this icon to quit."
        );
    }

    #[test]
    fn strings_fit_notifyicondata_limits() {
        assert_eq!(BALLOON_TITLE_WCHAR_CAP, 64);
        assert_eq!(BALLOON_BODY_WCHAR_CAP, 256);
        assert!(fits_wchar_z(BALLOON_TITLE, BALLOON_TITLE_WCHAR_CAP));
        assert!(fits_wchar_z(BALLOON_BODY, BALLOON_BODY_WCHAR_CAP));
        assert!(utf16_z_units(BALLOON_TITLE) < BALLOON_TITLE_WCHAR_CAP);
        assert!(utf16_z_units(BALLOON_BODY) < BALLOON_BODY_WCHAR_CAP);
        // ASCII, so bytes and UTF-16 units match, plus the NUL.
        assert_eq!(utf16_z_units(BALLOON_TITLE), BALLOON_TITLE.len() + 1);
        assert_eq!(utf16_z_units(BALLOON_BODY), BALLOON_BODY.len() + 1);
    }

    #[test]
    fn utf16_z_units_counts_a_surrogate_pair() {
        // U+1F4C4 is one scalar and two UTF-16 units, plus the NUL.
        assert_eq!(utf16_z_units("\u{1F4C4}"), 3);
        assert!(!fits_wchar_z("\u{1F4C4}", 2));
        assert!(fits_wchar_z("\u{1F4C4}", 3));
    }

    #[test]
    fn copy_utf16_z_round_trips_the_balloon_text() {
        let mut title = [0xFFFFu16; BALLOON_TITLE_WCHAR_CAP];
        let mut body = [0xFFFFu16; BALLOON_BODY_WCHAR_CAP];
        assert!(copy_utf16_z(&mut title, BALLOON_TITLE));
        assert!(copy_utf16_z(&mut body, BALLOON_BODY));
        assert_eq!(decode_z(&title), BALLOON_TITLE);
        assert_eq!(decode_z(&body), BALLOON_BODY);
        assert!(title[BALLOON_TITLE.len() + 1..]
            .iter()
            .all(|&unit| unit == 0));
        assert!(body[BALLOON_BODY.len() + 1..].iter().all(|&unit| unit == 0));
    }

    #[test]
    fn copy_utf16_z_rejects_overflow_without_writing() {
        let mut buf = [0xFFFFu16; 4];
        // Four units of text need a fifth slot for the NUL.
        assert!(!copy_utf16_z(&mut buf, "abcd"));
        assert_eq!(buf, [0xFFFF; 4]);
        assert!(copy_utf16_z(&mut buf, "abc"));
        assert_eq!(buf, [u16::from(b'a'), u16::from(b'b'), u16::from(b'c'), 0]);
    }

    #[test]
    fn copy_utf16_z_clears_the_tail() {
        let mut buf = [0xFFFFu16; 8];
        assert!(copy_utf16_z(&mut buf, "Hi"));
        assert_eq!(buf, [u16::from(b'H'), u16::from(b'i'), 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn registry_names_are_one_hkcu_value() {
        assert_eq!(FIRST_RUN_SUBKEY, "Software\\Spacepeek");
        assert_eq!(FIRST_RUN_VALUE_NAME, "FirstRunBalloonShown");
        assert_eq!(FIRST_RUN_SHOWN, 1);
        assert!(!FIRST_RUN_VALUE_NAME.contains('\\'));
        assert!(!FIRST_RUN_VALUE_NAME.contains('/'));
        assert_eq!(FIRST_RUN_SUBKEY.matches('\\').count(), 1);
    }

    #[test]
    fn shown_dword_suppresses_the_balloon() {
        assert!(!should_show_first_run_balloon(Some(FIRST_RUN_SHOWN)));
        assert!(!should_show_first_run_balloon(Some(1)));
        assert!(!should_show_first_run_balloon(interpret_dword_flag(
            true,
            true,
            4,
            FIRST_RUN_SHOWN
        )));
    }

    #[test]
    fn missing_or_other_values_show_the_balloon() {
        assert!(should_show_first_run_balloon(None));
        assert!(should_show_first_run_balloon(Some(0)));
        assert!(should_show_first_run_balloon(Some(2)));
        assert!(should_show_first_run_balloon(Some(u32::MAX)));
        assert!(should_show_first_run_balloon(interpret_dword_flag(
            false,
            true,
            4,
            FIRST_RUN_SHOWN
        )));
        assert!(should_show_first_run_balloon(interpret_dword_flag(
            true,
            false,
            4,
            FIRST_RUN_SHOWN
        )));
    }

    #[test]
    fn interpret_dword_flag_accepts_only_a_four_byte_dword() {
        assert_eq!(interpret_dword_flag(true, true, 4, 1), Some(1));
        assert_eq!(interpret_dword_flag(true, true, 4, 0), Some(0));
        assert_eq!(interpret_dword_flag(false, true, 4, 1), None);
        assert_eq!(interpret_dword_flag(true, false, 4, 1), None);
        assert_eq!(interpret_dword_flag(true, true, 0, 1), None);
        assert_eq!(interpret_dword_flag(true, true, 2, 1), None);
        assert_eq!(interpret_dword_flag(true, true, 8, 1), None);
        assert_eq!(interpret_dword_flag(false, false, 0, 0), None);
    }

    #[test]
    fn timeout_is_ten_seconds() {
        assert_eq!(BALLOON_TIMEOUT_MS, 10_000);
    }
}
