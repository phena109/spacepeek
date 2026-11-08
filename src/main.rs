use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;

use spacepeek::keys::should_intercept_space;
use spacepeek::preview::{load_text_preview, CancelToken, Generation, Preview};
use spacepeek::state::{step, Effect, Event, PreviewState};

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;
    use windows_sys::core::PWSTR;
    use windows_sys::Win32::Foundation::{
        HWND, HINSTANCE, HHOOK, LRESULT, LPARAM, RECT, SIZE, WPARAM, BOOL, HANDLE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongW, IsWindowVisible,
        SetWindowLongW, SetWindowPos, ShowWindow, GetMessageW, TranslateMessage, DispatchMessageW,
        GetFocus, GetForegroundWindow, GetClassNameW, GetKeyState, CallNextHookEx, GetGuiFocus,
        SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL, KBDLLHOOKSTRUCT,
        LLKHF_UP, VK_SPACE, WM_ACTIVATE, WM_CLOSE, WM_DESTROY, WM_ERASEBKGND, WM_PAINT,
        WM_SYSKEYDOWN, WM_SYSKEYUP, WM_KEYDOWN, WM_CHAR, WM_CTLCOLORSTATIC, ES_READONLY,
        WS_EX_TOPMOST, WS_EX_TOOLWINDOW, WS_POPUP, WS_VISIBLE, WS_BORDER, WS_OVERLAPPEDWINDOW,
        WS_EX_WINDOWEDGE, CLASS_TAKES_INPUT_FLAG, GWL_HWNDPROC, GWL_USERDATA, GWL_EXSTYLE,
        HWND_TOPMOST, HWND_MESSAGE, HWND_BROADCAST, SET_WINDOW_POS_FLAGS, SWP_ASYNCWINDOWPOS,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SHOW_WINDOW_CMD, WM_GETTOPMOST,
        WS_EX_NOACTIVATE,
    };
    use windows_sys::Win32::System::Com::CoInitializeEx;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE, CREATE_MUTEX_FLAGS};
    use windows_sys::Win32::UI::Shell::GetFocusedShellWindow;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState as GetKeyStateImpl, GetAsyncKeyState,
    };

    pub type HWNDWrapper = HWND;

    pub const CLASS_NAME: &str = "SpacepeekPreview";

    pub const WM_SPACEPEEK_SHOW: u32 = WM_USER + 1;
    pub const WM_SPACEPEEK_HIDE: u32 = WM_USER + 2;

    #[derive(Debug, Clone, Copy)]
    pub struct Rect {
        pub left: i32,
        pub top: i32,
        pub right: i32,
        pub bottom: i32,
    }

    impl Rect {
        fn empty() -> Self {
            Self { left: 0, top: 0, right: 0, bottom: 0 }
        }

        fn is_empty(&self) -> bool {
            self.left == 0 && self.top == 0 && self.right == 0 && self.bottom == 0
        }

        fn width(&self) -> i32 {
            (self.right - self.left) as i32
        }

        fn height(&self) -> i32 {
            (self.bottom - self.top) as i32
        }
    }

    pub fn run_windows_app(_tx: Sender<()>) {
        let _ = _tx;
    }
}

fn main() -> std::process::ExitCode {
    #[cfg(windows)]
    {
        let (tx, _rx) = channel();
        thread::spawn(move || {
            windows::run_windows_app(tx);
        });

        std::process::ExitCode::SUCCESS
    }

    #[cfg(not(windows))]
    {
        eprintln!("Spacepeek runs on Windows only");
        std::process::ExitCode::from(1)
    }
}
