#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    match spacepeek::parse_command(std::env::args()) {
        spacepeek::Command::Version => {
            println!("spacepeek {}", spacepeek::VERSION);
            std::process::ExitCode::SUCCESS
        }
        spacepeek::Command::Help => {
            eprintln!("Spacepeek {}", spacepeek::VERSION);
            eprintln!("Usage: spacepeek [--quit] [--version]");
            eprintln!("Spacepeek runs on Windows only");
            std::process::ExitCode::SUCCESS
        }
        _ => {
            eprintln!("Spacepeek runs on Windows only");
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    match win::run() {
        Ok(code) => code,
        Err(err) => {
            win::report(&format!("Spacepeek failed to start: {err}"));
            std::process::ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{
        AtomicBool, AtomicIsize, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering,
    };

    use spacepeek::{
        choose_selected, chrome_for_error, chrome_for_text, classify_typeahead_vk,
        fallback_type_name,
        close_button_bounds, decide_hook_key, double_click_action, effective_dpi, gate_show,
        hit_test, hook_key_from_vk, image_dest_rect, load_text_preview, min_physical_size,
        nav_reload_remaining, nonempty_path, parse_command, place_preview, pointer_over_close,
        scaled_close_width, scaled_detail_font, scaled_font_height, scaled_header_font,
        scaled_header_height, scaled_header_pad, session_phase, step, typeahead_window_ms,
        wheel_scroll_dy, CancelToken, ChromeLayout, Command, DecodedImage, DoubleClickAction,
        Effect, Event, Generation, HitZone, HookContext, HookDecision, HookKey, KeyPhase,
        Modifiers, NavKey, Presented, Preview, PreviewError, PreviewLimits, PreviewState, PxRect,
        QuickInfo,
        RepeatFilter, ScrollHold, ScrollOp, SessionPhase, ShowGate, ShowOutcome, SizeMemory,
        TrackedKey, Typeahead, TypeaheadKey, WindowKind, NAV_RELOAD_DELAY_MS, VERSION,
    };
    use windows::core::{w, Interface, PCWSTR, VARIANT};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, COLORREF, ERROR_ALREADY_EXISTS, ERROR_SUCCESS,
        E_FAIL, FILETIME, HANDLE, HINSTANCE, HMODULE, HWND, LPARAM, LRESULT, POINT, RECT, SYSTEMTIME,
        WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, ClientToScreen, CreateFontW, CreatePen, CreateSolidBrush, DeleteObject,
        DrawTextW, EndPaint, ExcludeClipRect, FillRect, GetDC, GetMonitorInfoW, GetSysColorBrush,
        GetTextFaceW, InvalidateRect, MonitorFromWindow, ReleaseDC, RestoreDC, RoundRect, SaveDC,
        SelectObject, SetBkMode, SetBrushOrgEx, SetStretchBltMode, SetTextColor, StretchDIBits,
        BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CLEARTYPE_QUALITY, COLOR_WINDOW, DEFAULT_CHARSET,
        DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_CENTER, DT_END_ELLIPSIS, DT_NOPREFIX,
        DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK, FF_MODERN, FF_SWISS, FIXED_PITCH, FW_NORMAL,
        FW_SEMIBOLD, HALFTONE, HDC, HFONT, HGDIOBJ, MONITORINFO, MONITOR_DEFAULTTONEAREST,
        PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT, VARIABLE_PITCH,
    };
    use windows::Win32::Globalization::{
        GetDateFormatEx, GetTimeFormatEx, DATE_LONGDATE, TIME_NOSECONDS,
    };
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_NORMAL, WIN32_FIND_DATAW};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IPersistFile,
        IServiceProvider, CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, STGM_READ,
    };
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Ole::IOleWindow;
    use windows::Win32::System::SystemInformation::GetTickCount;
    use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId};
    use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
    use windows::Win32::UI::HiDpi::{
        GetDpiForMonitor, GetDpiForWindow, GetSystemMetricsForDpi, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, GetCapture, GetDoubleClickTime, GetFocus, SetFocus, TrackMouseEvent,
        TME_LEAVE, TRACKMOUSEEVENT, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU, VK_RWIN,
        VK_SHIFT,
    };
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        FolderItem, FolderItems, IEnumIDList, IFolderView, IFolderView2, ILFree, IShellBrowser,
        IShellFolder, IShellFolderViewDual, IShellItem, IShellLinkW, IShellView, IShellWindows,
        IWebBrowser2, SHCreateItemWithParent, SHFILEINFOW, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES,
        SHGetFileInfoW, SID_STopLevelBrowser, ShellExecuteExW, ShellLink,
        ShellWindows, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD,
        NIM_DELETE, NIM_SETVERSION, NIN_SELECT, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
        SEE_MASK_FLAG_NO_UI, SHELLEXECUTEINFOW, SIGDN_FILESYSPATH, SLGP_RAWPATH, SVGIO_SELECTION,
        SVSI_SELECT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, BringWindowToTop, CallNextHookEx, CallWindowProcW, CreatePopupMenu,
        CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow, DispatchMessageW,
        FindWindowExW, FindWindowW, GetAncestor, GetClassNameW, GetClientRect, GetCursorPos,
        GetForegroundWindow, GetGUIThreadInfo, GetMessageW, GetParent, GetSystemMetrics,
        GetWindowRect, GetWindowThreadProcessId, HideCaret, IsWindow, IsWindowVisible, KillTimer,
        LoadCursorW, LoadIconW, MessageBoxW, MoveWindow, PeekMessageW, PostMessageW,
        PostQuitMessage, PostThreadMessageW, RegisterClassW, RegisterWindowMessageW, SendMessageW,
        SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW,
        SetWindowsHookExW, ShowWindow, TrackPopupMenu, TranslateMessage, UnhookWindowsHookEx,
        CS_DBLCLKS, CS_HREDRAW, CS_VREDRAW, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE,
        ES_READONLY, EVENT_SYSTEM_FOREGROUND, GA_ROOT, GUITHREADINFO, GWLP_WNDPROC, HHOOK, HICON,
        HMENU, HTBOTTOM, HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCAPTION, HTCLIENT, HTLEFT, HTRIGHT, HTTOP,
        HTTOPLEFT, HTTOPRIGHT, IDC_ARROW, IDI_APPLICATION, KBDLLHOOKSTRUCT, LLKHF_UP,
        MA_NOACTIVATE, MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MESSAGEBOX_STYLE, MF_STRING,
        MINMAXINFO, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, SB_BOTTOM, SB_PAGEDOWN, SB_PAGEUP, SB_TOP,
        SM_CXFRAME, SM_CXPADDEDBORDER, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOZORDER,
        SW_HIDE, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE, WINDOW_STYLE, WINEVENT_OUTOFCONTEXT, WM_APP,
        WM_CHAR, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
        WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN,
        WM_LBUTTONUP, WM_MBUTTONDBLCLK, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEACTIVATE,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCMOUSEMOVE, WM_NULL,
        WM_PAINT, WM_QUIT, WM_RBUTTONDBLCLK, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETFOCUS, WM_SETFONT,
        WM_SIZE, WM_SYSKEYDOWN, WM_SYSKEYUP, WM_TIMER, WM_USER, WM_VSCROLL, WNDCLASSW,
        WNDCLASS_STYLES, WS_CHILD, WS_CLIPCHILDREN, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
        WS_EX_TOPMOST, WS_HSCROLL, WS_POPUP, WS_THICKFRAME, WS_VISIBLE, WS_VSCROLL,
    };

    const PREVIEW_CLASS: &str = "SpacepeekPreview";
    const MESSAGE_CLASS: &str = "SpacepeekMessage";
    const WM_SPACEPEEK_SPACE: u32 = WM_APP + 1;
    const WM_SPACEPEEK_ESC: u32 = WM_APP + 2;
    const WM_SPACEPEEK_LOADED: u32 = WM_APP + 3;
    const WM_SPACEPEEK_TRAY: u32 = WM_APP + 4;
    const WM_SPACEPEEK_NAV: u32 = WM_APP + 5;
    const WM_SPACEPEEK_SCROLL: u32 = WM_APP + 6;
    const WM_SPACEPEEK_WHEEL: u32 = WM_APP + 7;
    const WM_SPACEPEEK_FOREGROUND: u32 = WM_APP + 8;
    /// Posted from the edit control so a body double-click opens after that
    /// procedure returns. The window is gone before a triple-click can repeat it.
    const WM_SPACEPEEK_OPEN: u32 = WM_APP + 9;
    /// `NIN_KEYSELECT` is `WM_USER + 1`. The windows crate exports `NIN_SELECT` only.
    const NIN_KEYSELECT: u32 = WM_USER + 1;
    const EM_SETLIMITTEXT: u32 = 0x00C5;
    /// `EM_LINESCROLL`. Kept local so the edit control does not need the Controls feature.
    const EM_LINESCROLL: u32 = 0x00B6;
    /// `WM_MOUSELEAVE`. Kept local so the preview does not need the Controls feature.
    const WM_MOUSELEAVE: u32 = 0x02A3;
    const NAV_TIMER_ID: usize = 1;
    const WATCH_TIMER_ID: usize = 2;
    /// Backup for a missed foreground event while a session is open.
    const WATCH_INTERVAL_MS: u32 = 250;
    const WHEEL_LINES_PER_NOTCH: i32 = 3;
    const TRAY_ID: u32 = 1;
    const CMD_EXIT: usize = 1;
    const ALREADY_RUNNING: &str = "Spacepeek is already running. Right-click its icon in the notification area and choose Exit, or run spacepeek.exe --quit.";
    const NOT_RUNNING: &str = "Spacepeek is not running.";

    fn usage_text() -> String {
        format!(
            "Spacepeek {VERSION}\r\n\r\nUsage: spacepeek.exe [--quit] [--version]\r\n\r\nStart spacepeek.exe and leave it running. spacepeek.exe --quit asks the running instance to exit. spacepeek.exe --version prints the build version."
        )
    }

    struct LoadMsg {
        gen: Generation,
        result: std::result::Result<Preview, PreviewError>,
    }

    struct App {
        state: PreviewState,
        cancel: CancelToken,
        msg_hwnd: HWND,
        preview_hwnd: HWND,
        edit_hwnd: HWND,
        explorer_hwnd: HWND,
        instance: HINSTANCE,
        font: HFONT,
        header_font: HFONT,
        detail_font: HFONT,
        font_dpi: u32,
        /// Last user-chosen outer size, in logical pixels. Empty until a resize.
        sizes: SizeMemory,
        /// Header and, for errors, the centred message. The edit holds file text.
        presented: Option<Presented>,
        /// Set for the duration of `on_space`. Explorer COM calls pump this STA
        /// thread, so a posted Space can otherwise re-enter `on_space`.
        space_busy: bool,
        /// `INPUT_GEN` at the moment the current load started. A Space or Esc
        /// posted after that must not be followed by a show.
        input_baseline: u64,
        /// Tick of the latest arrow that should reload the selection.
        last_nav_tick: Option<u32>,
        /// `selected_path` pumps the STA. Ignore a nested navigation read.
        nav_busy: bool,
        /// Pointer is inside the × hit-test area. Paint uses this; only a change
        /// invalidates, and only the button rect.
        close_hovered: bool,
        /// Filesystem path of the file the window is showing. Empty when hidden.
        /// A reload keeps the previous path until the new content is installed.
        shown_path: PathBuf,
    }

    static APP: AtomicPtr<RefCell<App>> = AtomicPtr::new(std::ptr::null_mut());
    static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
    static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);
    static TRAY_V4: AtomicBool = AtomicBool::new(false);
    static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
    /// Preview HWND for the hook thread. Zero when there is no preview window.
    static PREVIEW_HWND: AtomicIsize = AtomicIsize::new(0);
    /// [`SessionPhase`] as a byte. The hook thread reads it.
    static PHASE: AtomicU8 = AtomicU8::new(0);
    /// Explorer that owns the current session. Zero when nothing is open.
    static ORIGIN_EXPLORER: AtomicIsize = AtomicIsize::new(0);
    /// Bumped when Space or Esc is posted to the UI thread.
    static INPUT_GEN: AtomicU64 = AtomicU64::new(0);
    /// Previous edit-control window procedure. Zero when the edit is gone.
    static EDIT_PROC: AtomicIsize = AtomicIsize::new(0);
    static FOREGROUND_HOOK: AtomicIsize = AtomicIsize::new(0);
    // Hook-thread only. The low-level hook runs on `spacepeek-hook`.
    thread_local! {
        static REPEATS: RefCell<RepeatFilter> = const { RefCell::new(RepeatFilter::new()) };
        static TYPEAHEAD: RefCell<Typeahead> = const { RefCell::new(Typeahead::new(1_000)) };
        static SCROLLS: RefCell<ScrollHold> = const { RefCell::new(ScrollHold::new()) };
    }

    struct HookThread {
        id: u32,
        join: std::thread::JoinHandle<()>,
    }

    pub fn run() -> windows::core::Result<std::process::ExitCode> {
        match parse_command(std::env::args()) {
            Command::Quit => return quit_running(),
            Command::Help => {
                info_box(&usage_text());
                return Ok(std::process::ExitCode::SUCCESS);
            }
            Command::Version => {
                let line = format!("spacepeek {VERSION}\n");
                if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok() {
                    use std::io::Write;
                    let mut stdout = std::io::stdout();
                    let _ = stdout.write_all(line.as_bytes());
                    let _ = stdout.flush();
                } else {
                    info_box(line.trim_end());
                }
                return Ok(std::process::ExitCode::SUCCESS);
            }
            Command::Unknown => {
                info_box(&usage_text());
                return Ok(std::process::ExitCode::from(1));
            }
            Command::Run => {}
        }

        let held = match acquire_singleton() {
            Ok(Some(handle)) => handle,
            Ok(None) => {
                info_box(ALREADY_RUNNING);
                return Ok(std::process::ExitCode::from(1));
            }
            Err(err) => return Err(err),
        };

        let module = unsafe { GetModuleHandleW(PCWSTR::null())? };
        let instance = HINSTANCE::from(module);
        let app = Box::leak(Box::new(RefCell::new(App {
            state: PreviewState::Hidden,
            cancel: CancelToken::new(),
            msg_hwnd: HWND::default(),
            preview_hwnd: HWND::default(),
            edit_hwnd: HWND::default(),
            explorer_hwnd: HWND::default(),
            instance,
            font: HFONT::default(),
            header_font: HFONT::default(),
            detail_font: HFONT::default(),
            font_dpi: 0,
            sizes: SizeMemory::new(),
            presented: None,
            space_busy: false,
            input_baseline: 0,
            last_nav_tick: None,
            nav_busy: false,
            close_hovered: false,
            shown_path: PathBuf::new(),
        })));
        APP.store(app, Ordering::Release);

        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register_class(instance, MESSAGE_CLASS, CS_HREDRAW | CS_VREDRAW)?;
            // `CS_DBLCLKS` so an image or message body (drawn by this window,
            // not an edit child) receives `WM_LBUTTONDBLCLK`.
            register_class(
                instance,
                PREVIEW_CLASS,
                CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            )?;
            let msg_hwnd = create_message_window(instance)?;
            app.borrow_mut().msg_hwnd = msg_hwnd;
            MAIN_HWND.store(msg_hwnd.0 as isize, Ordering::Release);
            register_taskbar_created();
            add_tray_icon(msg_hwnd);
            install_foreground_hook();

            // The hook cannot share this thread: a slow Explorer COM call on the
            // STA would stall every keystroke, and Windows may silently unhook it.
            let hook = match start_hook_thread(instance) {
                Ok(hook) => hook,
                Err(err) => {
                    remove_foreground_hook();
                    remove_tray_icon(msg_hwnd);
                    let _ = DestroyWindow(msg_hwnd);
                    CoUninitialize();
                    return Err(err);
                }
            };
            let mut msg = MSG::default();
            loop {
                let rc = GetMessageW(&mut msg, HWND::default(), 0, 0);
                if rc.0 == 0 || rc.0 == -1 {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            stop_hook_thread(hook);
            remove_foreground_hook();
            CoUninitialize();
        }

        let _held = held;
        Ok(std::process::ExitCode::SUCCESS)
    }

    pub fn report(text: &str) {
        message_box(text, MB_OK | MB_ICONERROR);
    }

    fn info_box(text: &str) {
        message_box(text, MB_OK | MB_ICONINFORMATION);
    }

    fn message_box(text: &str, style: MESSAGEBOX_STYLE) {
        let body = utf16(text);
        let title = utf16("Spacepeek");
        unsafe {
            MessageBoxW(
                HWND::default(),
                PCWSTR(body.as_ptr()),
                PCWSTR(title.as_ptr()),
                style,
            );
        }
    }

    fn quit_running() -> windows::core::Result<std::process::ExitCode> {
        let class_name = utf16(MESSAGE_CLASS);
        let found = unsafe { FindWindowW(PCWSTR(class_name.as_ptr()), PCWSTR::null()) };
        let Ok(hwnd) = found else {
            info_box(NOT_RUNNING);
            return Ok(std::process::ExitCode::from(1));
        };
        unsafe { PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0))? };
        Ok(std::process::ExitCode::SUCCESS)
    }

    fn acquire_singleton() -> windows::core::Result<Option<HANDLE>> {
        unsafe {
            if let Some(handle) = try_mutex(w_global()) {
                return Ok(handle);
            }
            if let Some(handle) = try_mutex(w_local()) {
                return Ok(handle);
            }
            Err(windows::core::Error::from_win32())
        }
    }

    fn w_global() -> Vec<u16> {
        utf16("Global\\SpacepeekSingleton")
    }

    fn w_local() -> Vec<u16> {
        utf16("Local\\SpacepeekSingleton")
    }

    unsafe fn try_mutex(name: Vec<u16>) -> Option<Option<HANDLE>> {
        SetLastError(ERROR_SUCCESS);
        let handle = CreateMutexW(None, true, PCWSTR(name.as_ptr())).ok()?;
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let _ = CloseHandle(handle);
            Some(None)
        } else {
            Some(Some(handle))
        }
    }

    fn app() -> &'static RefCell<App> {
        let ptr = APP.load(Ordering::Acquire);
        debug_assert!(!ptr.is_null());
        unsafe { &*ptr }
    }

    fn with_app<R>(f: impl FnOnce(&App) -> R) -> R {
        f(&app().borrow())
    }

    fn with_app_mut<R>(f: impl FnOnce(&mut App) -> R) -> R {
        f(&mut app().borrow_mut())
    }

    fn main_hwnd() -> HWND {
        HWND(MAIN_HWND.load(Ordering::Acquire) as *mut _)
    }

    fn start_hook_thread(instance: HINSTANCE) -> windows::core::Result<HookThread> {
        let (tx, rx) = std::sync::mpsc::channel();
        // Handles are raw pointers and not Send. Pass the bits across the thread.
        let instance_bits = instance.0 as isize;
        let join = std::thread::Builder::new()
            .name("spacepeek-hook".to_string())
            .spawn(move || hook_thread_main(instance_bits, tx))
            .map_err(windows::core::Error::from)?;
        match rx.recv() {
            Ok(Ok(id)) => Ok(HookThread { id, join }),
            Ok(Err(err)) => {
                let _ = join.join();
                Err(err)
            }
            Err(_) => {
                let _ = join.join();
                Err(windows::core::Error::new(
                    E_FAIL,
                    "hook thread exited before installing the keyboard hook",
                ))
            }
        }
    }

    fn stop_hook_thread(hook: HookThread) {
        unsafe {
            let _ = PostThreadMessageW(hook.id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        let _ = hook.join.join();
    }

    fn hook_thread_main(
        instance_bits: isize,
        tx: std::sync::mpsc::Sender<windows::core::Result<u32>>,
    ) {
        unsafe {
            let instance = HINSTANCE(instance_bits as *mut _);
            let mut msg = MSG::default();
            // The queue must exist before the UI thread can post WM_QUIT.
            let _ = PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_NOREMOVE);
            let id = GetCurrentThreadId();
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), instance, 0) {
                Ok(hook) => hook,
                Err(err) => {
                    let _ = tx.send(Err(err));
                    return;
                }
            };
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), instance, 0).ok();
            if mouse.is_none() {
                debug_log("Spacepeek: mouse wheel hook was not installed\n");
            }
            if tx.send(Ok(id)).is_err() {
                if let Some(mouse) = mouse {
                    let _ = UnhookWindowsHookEx(mouse);
                }
                let _ = UnhookWindowsHookEx(hook);
                return;
            }
            loop {
                let rc = GetMessageW(&mut msg, HWND::default(), 0, 0);
                if rc.0 == 0 || rc.0 == -1 {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if let Some(mouse) = mouse {
                let _ = UnhookWindowsHookEx(mouse);
            }
            let _ = UnhookWindowsHookEx(hook);
        }
    }

    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code < 0 {
            return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
        }
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let vk = kb.vkCode as u16;
        let up = kb.flags.contains(LLKHF_UP);
        let phase = if up { KeyPhase::Up } else { KeyPhase::Down };
        let Some(key) = hook_key_from_vk(vk) else {
            if !up {
                note_typeahead(vk, kb.time);
            }
            return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
        };
        match key {
            HookKey::Nav(nav) => handle_nav(code, wparam, lparam, nav, phase, kb.time),
            HookKey::Space => handle_toggle(
                code,
                wparam,
                lparam,
                HookKey::Space,
                TrackedKey::Space,
                phase,
                kb.time,
            ),
            HookKey::Esc => handle_toggle(
                code,
                wparam,
                lparam,
                HookKey::Esc,
                TrackedKey::Esc,
                phase,
                kb.time,
            ),
        }
    }

    fn handle_nav(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
        nav: NavKey,
        phase: KeyPhase,
        tick: u32,
    ) -> LRESULT {
        let ctx = hook_context(tick);
        let decision = decide_hook_key(&ctx, HookKey::Nav(nav));
        if phase == KeyPhase::Down {
            note_nav_typeahead(&ctx, &decision);
        }
        match (phase, decision) {
            (KeyPhase::Down, HookDecision::Scroll(op)) => {
                let repeat =
                    SCROLLS.with(|slot| slot.borrow_mut().observe(nav, KeyPhase::Down, true));
                if repeat.act && !post_app_param(WM_SPACEPEEK_SCROLL, usize::from(op.code())) {
                    SCROLLS.with(|slot| slot.borrow_mut().release(nav));
                    return call_next(code, wparam, lparam);
                }
                if repeat.swallow {
                    return LRESULT(1);
                }
            }
            (KeyPhase::Down, HookDecision::FollowSelection) => {
                // Explorer still has focus, so the key moves the selection.
                // The UI re-reads it after the debounce.
                let _ = post_app(WM_SPACEPEEK_NAV);
            }
            _ => {
                let take = matches!(decision, HookDecision::Scroll(_));
                let repeat = SCROLLS.with(|slot| slot.borrow_mut().observe(nav, phase, take));
                if repeat.swallow {
                    return LRESULT(1);
                }
            }
        }
        call_next(code, wparam, lparam)
    }

    fn note_nav_typeahead(ctx: &HookContext, decision: &HookDecision) {
        if !ctx.explorer_list_focused {
            TYPEAHEAD.with(|slot| slot.borrow_mut().clear());
            return;
        }
        if ctx.modifiers.command() {
            return;
        }
        // Arrows end Explorer's search. Page keys are swallowed, but the search
        // is over either way, so the next Space can open a preview again.
        if matches!(
            decision,
            HookDecision::FollowSelection | HookDecision::Scroll(_) | HookDecision::Pass
        ) {
            TYPEAHEAD.with(|slot| slot.borrow_mut().observe(TypeaheadKey::Cancels, 0));
        }
    }

    fn handle_toggle(
        code: i32,
        wparam: WPARAM,
        lparam: LPARAM,
        key: HookKey,
        tracked: TrackedKey,
        phase: KeyPhase,
        tick: u32,
    ) -> LRESULT {
        let ctx = hook_context(tick);
        let decision = decide_hook_key(&ctx, key);
        if phase == KeyPhase::Down {
            note_toggle_typeahead(&ctx, key, &decision, tick);
        }
        // CancelPending posts and does not swallow, so Explorer still sees Esc.
        // The repeat filter would swallow the rest of the press, so the latch
        // is cleared after a successful post.
        let (intercept, pass_through) = match decision {
            HookDecision::Space | HookDecision::Dismiss => (true, false),
            HookDecision::CancelPending => (true, true),
            _ => (false, false),
        };
        let intercept = if phase == KeyPhase::Up {
            false
        } else {
            intercept
        };
        let mut repeat = REPEATS.with(|slot| slot.borrow_mut().observe(tracked, phase, intercept));
        if repeat.act {
            let msg = match tracked {
                TrackedKey::Space => WM_SPACEPEEK_SPACE,
                TrackedKey::Esc => WM_SPACEPEEK_ESC,
            };
            if !post_app(msg) {
                REPEATS.with(|slot| slot.borrow_mut().set_swallowed(tracked, false));
                return call_next(code, wparam, lparam);
            }
            INPUT_GEN.fetch_add(1, Ordering::Release);
            if pass_through {
                REPEATS.with(|slot| slot.borrow_mut().set_swallowed(tracked, false));
                repeat.swallow = false;
            }
        }
        if repeat.swallow {
            return LRESULT(1);
        }
        call_next(code, wparam, lparam)
    }

    fn note_toggle_typeahead(ctx: &HookContext, key: HookKey, decision: &HookDecision, tick: u32) {
        if !ctx.explorer_list_focused {
            TYPEAHEAD.with(|slot| slot.borrow_mut().clear());
            return;
        }
        if ctx.modifiers.command() {
            return;
        }
        match (key, decision) {
            (HookKey::Esc, _) => {
                TYPEAHEAD.with(|slot| slot.borrow_mut().observe(TypeaheadKey::Cancels, tick));
            }
            (HookKey::Space, HookDecision::Pass) if ctx.typeahead_active => {
                TYPEAHEAD.with(|slot| slot.borrow_mut().observe(TypeaheadKey::PassedSpace, tick));
            }
            _ => {}
        }
    }

    unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code < 0 {
            return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
        }
        if wparam.0 as u32 != WM_MOUSEWHEEL {
            return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
        }
        let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        if !point_in_preview(ms.pt) {
            return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
        }
        let delta = i32::from((ms.mouseData >> 16) as i16);
        if delta != 0 && post_app_param(WM_SPACEPEEK_WHEEL, delta as u32 as usize) {
            return LRESULT(1);
        }
        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    fn call_next(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        unsafe { CallNextHookEx(HHOOK::default(), code, wparam, lparam) }
    }

    fn hook_context(tick: u32) -> HookContext {
        let modifiers = read_modifiers();
        let explorer_list_focused = explorer_list_focused();
        let typeahead_active = explorer_list_focused
            && TYPEAHEAD.with(|slot| {
                let mut typeahead = slot.borrow_mut();
                typeahead.set_window_ms(typeahead_window_ms(unsafe { GetDoubleClickTime() }));
                typeahead.active(tick)
            });
        HookContext {
            phase: current_phase(),
            explorer_list_focused,
            preview_is_foreground: preview_is_foreground(),
            origin_is_foreground: origin_is_foreground(),
            modifiers,
            typeahead_active,
        }
    }

    fn note_typeahead(vk: u16, tick: u32) {
        let modifiers = read_modifiers();
        let kind = classify_typeahead_vk(vk, modifiers);
        if kind == TypeaheadKey::Ignored {
            return;
        }
        let list = explorer_list_focused();
        TYPEAHEAD.with(|slot| {
            let mut typeahead = slot.borrow_mut();
            if !list {
                typeahead.clear();
                return;
            }
            typeahead.set_window_ms(typeahead_window_ms(unsafe { GetDoubleClickTime() }));
            typeahead.observe(kind, tick);
        });
    }

    fn current_phase() -> SessionPhase {
        SessionPhase::from_code(PHASE.load(Ordering::Acquire))
    }

    fn publish_session(state: &PreviewState, explorer: HWND) {
        let phase = session_phase(state);
        let origin = if phase.is_hidden() {
            0
        } else {
            explorer.0 as isize
        };
        // Window first, then the phase, so a hook that sees a session also sees it.
        ORIGIN_EXPLORER.store(origin, Ordering::Release);
        PHASE.store(phase as u8, Ordering::Release);
    }

    fn sync_watch_timer() {
        let hwnd = main_hwnd();
        if hwnd.0.is_null() {
            return;
        }
        unsafe {
            if current_phase().is_hidden() {
                let _ = KillTimer(hwnd, WATCH_TIMER_ID);
            } else {
                let _ = SetTimer(hwnd, WATCH_TIMER_ID, WATCH_INTERVAL_MS, None);
            }
        }
    }

    fn origin_is_foreground() -> bool {
        let bits = ORIGIN_EXPLORER.load(Ordering::Acquire);
        if bits == 0 {
            return false;
        }
        let explorer = HWND(bits as *mut _);
        unsafe { IsWindow(explorer).as_bool() && GetForegroundWindow() == explorer }
    }

    fn preview_is_foreground() -> bool {
        let bits = PREVIEW_HWND.load(Ordering::Acquire);
        if bits == 0 {
            return false;
        }
        let preview = HWND(bits as *mut _);
        unsafe {
            IsWindow(preview).as_bool()
                && IsWindowVisible(preview).as_bool()
                && GetForegroundWindow() == preview
        }
    }

    fn explorer_list_focused() -> bool {
        let foreground = unsafe { GetForegroundWindow() };
        if !class_eq(foreground, "CabinetWClass") {
            return false;
        }
        class_eq(focus_hwnd(foreground), "DirectUIHWND")
    }

    fn publish_preview_hwnd(hwnd: HWND) {
        PREVIEW_HWND.store(hwnd.0 as isize, Ordering::Release);
    }

    fn post_app(msg: u32) -> bool {
        post_app_param(msg, 0)
    }

    fn post_app_param(msg: u32, wparam: usize) -> bool {
        let hwnd = main_hwnd();
        if hwnd.0.is_null() {
            return false;
        }
        unsafe { PostMessageW(hwnd, msg, WPARAM(wparam), LPARAM(0)).is_ok() }
    }

    fn read_modifiers() -> Modifiers {
        fn down(vk: VIRTUAL_KEY) -> bool {
            unsafe { GetAsyncKeyState(i32::from(vk.0)) < 0 }
        }
        Modifiers {
            ctrl: down(VK_CONTROL),
            alt: down(VK_MENU),
            win: down(VK_LWIN) || down(VK_RWIN),
            shift: down(VK_SHIFT),
        }
    }

    fn point_in_preview(pt: POINT) -> bool {
        let bits = PREVIEW_HWND.load(Ordering::Acquire);
        if bits == 0 {
            return false;
        }
        let hwnd = HWND(bits as *mut _);
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                return false;
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                return false;
            }
            pt.x >= rect.left && pt.x < rect.right && pt.y >= rect.top && pt.y < rect.bottom
        }
    }

    fn install_foreground_hook() {
        unsafe {
            let hook = SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                HMODULE(std::ptr::null_mut()),
                Some(foreground_event),
                0,
                0,
                WINEVENT_OUTOFCONTEXT,
            );
            FOREGROUND_HOOK.store(hook.0 as isize, Ordering::Release);
            if hook.0.is_null() {
                debug_log("Spacepeek: foreground hook was not installed\n");
            }
        }
    }

    fn remove_foreground_hook() {
        let bits = FOREGROUND_HOOK.swap(0, Ordering::AcqRel);
        if bits == 0 {
            return;
        }
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(bits as *mut _));
        }
    }

    unsafe extern "system" fn foreground_event(
        _hook: HWINEVENTHOOK,
        event: u32,
        hwnd: HWND,
        _id_object: i32,
        _id_child: i32,
        _thread: u32,
        _time: u32,
    ) {
        if event != EVENT_SYSTEM_FOREGROUND || hwnd.0.is_null() {
            return;
        }
        let root = GetAncestor(hwnd, GA_ROOT);
        let hwnd = if root.0.is_null() { hwnd } else { root };
        let _ = post_app_param(WM_SPACEPEEK_FOREGROUND, hwnd.0 as usize);
    }

    fn focus_hwnd(foreground: HWND) -> HWND {
        if foreground.0.is_null() {
            return HWND::default();
        }
        unsafe {
            let thread = GetWindowThreadProcessId(foreground, None);
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..GUITHREADINFO::default()
            };
            if GetGUIThreadInfo(thread, &mut info).is_ok() && !info.hwndFocus.0.is_null() {
                return info.hwndFocus;
            }
            let focus = GetFocus();
            if !focus.0.is_null() {
                return focus;
            }
            foreground
        }
    }

    fn class_eq(hwnd: HWND, expected: &str) -> bool {
        if hwnd.0.is_null() {
            return false;
        }
        let mut buf = [0u16; 64];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        if n <= 0 {
            return false;
        }
        expected
            .encode_utf16()
            .eq(buf[..n as usize].iter().copied())
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if APP.load(Ordering::Acquire).is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let (msg_hwnd, preview_hwnd) = with_app(|app| (app.msg_hwnd, app.preview_hwnd));
        if hwnd == msg_hwnd && !msg_hwnd.0.is_null() {
            return message_proc(hwnd, msg, wparam, lparam);
        }
        if hwnd == preview_hwnd && !preview_hwnd.0.is_null() {
            return preview_proc(hwnd, msg, wparam, lparam);
        }
        DefWindowProcW(hwnd, msg, wparam, lparam)
    }

    fn message_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let taskbar = TASKBAR_CREATED.load(Ordering::Acquire);
        if taskbar != 0 && msg == taskbar {
            add_tray_icon(hwnd);
            return LRESULT(0);
        }
        match msg {
            WM_SPACEPEEK_SPACE => {
                on_space();
                LRESULT(0)
            }
            WM_SPACEPEEK_ESC => {
                dispatch(Event::EscPressed);
                LRESULT(0)
            }
            WM_SPACEPEEK_LOADED => {
                on_loaded(lparam);
                LRESULT(0)
            }
            WM_SPACEPEEK_NAV => {
                on_nav_request();
                LRESULT(0)
            }
            WM_SPACEPEEK_SCROLL => {
                if let Some(op) = ScrollOp::from_code(wparam.0 as u8) {
                    scroll_preview(op);
                }
                LRESULT(0)
            }
            WM_SPACEPEEK_WHEEL => {
                scroll_wheel(wparam.0 as u32 as i32);
                LRESULT(0)
            }
            WM_SPACEPEEK_FOREGROUND => {
                on_foreground_hwnd(HWND(wparam.0 as *mut _));
                LRESULT(0)
            }
            WM_TIMER => {
                match wparam.0 {
                    NAV_TIMER_ID => on_nav_timer(),
                    WATCH_TIMER_ID => on_watch_timer(),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_SPACEPEEK_TRAY => {
                on_tray_message(hwnd, wparam, lparam);
                LRESULT(0)
            }
            WM_CLOSE => {
                begin_shutdown(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                remove_tray_icon(hwnd);
                MAIN_HWND.store(0, Ordering::Release);
                unsafe { PostQuitMessage(0) };
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn preview_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            // Header drag and edge resize. The × button stays client so the
            // click is ours; returning HTCAPTION there would start a drag.
            // MA_NOACTIVATE and WS_EX_NOACTIVATE keep Explorer in front. A
            // caption drag that still activates is handed back on WM_EXITSIZEMOVE.
            WM_NCHITTEST => LRESULT(zone_to_hit(hit_at_screen(hwnd, lparam)) as isize),
            WM_GETMINMAXINFO => {
                apply_min_track(hwnd, lparam);
                LRESULT(0)
            }
            WM_EXITSIZEMOVE => {
                remember_window_size(hwnd);
                let explorer = with_app(|app| app.explorer_hwnd);
                restore_explorer_focus(explorer);
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if hit_at_client(hwnd, lparam) == HitZone::Close {
                    dispatch(Event::EscPressed);
                }
                LRESULT(0)
            }
            // The × is client area (`HTCLIENT`). The rest of the header is
            // `HTCAPTION`, so leaving the button is either another client move
            // or a non-client move. `TME_LEAVE` covers the window edge.
            WM_MOUSEMOVE => {
                let (x, y) = client_point_in_window(hwnd, lparam);
                update_close_hover(hwnd, pointer_over_close(window_layout(hwnd), x, y));
                track_client_leave(hwnd);
                LRESULT(0)
            }
            WM_NCMOUSEMOVE => {
                let (x, y) = screen_point_in_window(hwnd, lparam);
                update_close_hover(hwnd, pointer_over_close(window_layout(hwnd), x, y));
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                update_close_hover(hwnd, false);
                LRESULT(0)
            }
            // Body only. The header is non-client, and the × is `HitZone::Close`.
            WM_LBUTTONDBLCLK => {
                if double_click_action(hit_at_client(hwnd, lparam)) == DoubleClickAction::Open {
                    open_current_file();
                }
                LRESULT(0)
            }
            // `CS_DBLCLKS` also turns a header double-click into this message.
            // DefWindowProc would maximize. The header does not open the file.
            WM_NCLBUTTONDBLCLK => LRESULT(0),
            WM_SPACEPEEK_OPEN => {
                open_current_file();
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                paint_preview(hwnd);
                LRESULT(0)
            }
            WM_SIZE => {
                resize_edit(hwnd);
                invalidate_preview(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE.0) => {
                dispatch(Event::EscPressed);
                LRESULT(0)
            }
            WM_CLOSE => {
                dispatch(Event::EscPressed);
                LRESULT(0)
            }
            WM_DPICHANGED => {
                on_preview_dpi_changed(hwnd, wparam, lparam);
                LRESULT(0)
            }
            // Clicks must not activate the preview. Scrollbar input still arrives.
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_SETFOCUS => {
                let edit = with_app(|app| app.edit_hwnd);
                unsafe {
                    let _ = HideCaret(edit);
                }
                // A header click can still focus us. Put Explorer back so arrow
                // keys keep moving the selection. Skip when Explorer is already
                // in front, and skip while a size loop has capture.
                if unsafe { GetCapture() } == HWND::default() {
                    let explorer = with_app(|app| app.explorer_hwnd);
                    restore_explorer_focus(explorer);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                with_app_mut(|app| {
                    if app.preview_hwnd == hwnd {
                        app.preview_hwnd = HWND::default();
                        app.edit_hwnd = HWND::default();
                        app.presented = None;
                        app.close_hovered = false;
                        app.shown_path.clear();
                        publish_preview_hwnd(HWND::default());
                    }
                });
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn on_space() {
        let busy = with_app_mut(|app| {
            if app.space_busy {
                true
            } else {
                app.space_busy = true;
                false
            }
        });
        if busy {
            return;
        }
        let _clear = SpaceBusyGuard;
        let visible = with_app(|app| !matches!(app.state, PreviewState::Hidden));
        if visible {
            dispatch(Event::SpacePressed(PathBuf::new()));
            return;
        }
        let explorer = unsafe { GetForegroundWindow() };
        with_app_mut(|app| app.explorer_hwnd = explorer);
        let path = nonempty_path(unsafe { selected_path(explorer) });
        // The hook cannot query the selection. That COM call belongs on this
        // STA thread; doing it inside the low-level hook would stall every
        // keystroke. Space was already swallowed. With no filesystem path,
        // opening a window is the failure — do nothing. Re-injecting the key
        // would re-enter the hook.
        if let Some(path) = path {
            dispatch(Event::SpacePressed(path));
        }
    }

    struct SpaceBusyGuard;

    impl Drop for SpaceBusyGuard {
        fn drop(&mut self) {
            with_app_mut(|app| app.space_busy = false);
        }
    }

    fn on_loaded(lparam: LPARAM) {
        let msg = unsafe { Box::from_raw(lparam.0 as *mut LoadMsg) };
        dispatch(Event::LoadFinished(msg.gen, msg.result));
    }

    fn on_nav_request() {
        if current_phase().is_hidden() {
            return;
        }
        let now = unsafe { GetTickCount() };
        with_app_mut(|app| app.last_nav_tick = Some(now));
        schedule_nav_timer(NAV_RELOAD_DELAY_MS);
    }

    fn on_nav_timer() {
        let now = unsafe { GetTickCount() };
        let remaining =
            with_app(|app| nav_reload_remaining(app.last_nav_tick, now, NAV_RELOAD_DELAY_MS));
        match remaining {
            Some(0) => {
                with_app_mut(|app| app.last_nav_tick = None);
                let hwnd = main_hwnd();
                unsafe {
                    let _ = KillTimer(hwnd, NAV_TIMER_ID);
                }
                reload_selection();
            }
            Some(ms) => schedule_nav_timer(ms.max(1)),
            None => {
                let hwnd = main_hwnd();
                unsafe {
                    let _ = KillTimer(hwnd, NAV_TIMER_ID);
                }
            }
        }
    }

    fn schedule_nav_timer(delay_ms: u32) {
        let hwnd = main_hwnd();
        if hwnd.0.is_null() {
            return;
        }
        let id = unsafe { SetTimer(hwnd, NAV_TIMER_ID, delay_ms, None) };
        if id == 0 {
            debug_log("Spacepeek: navigation timer was not set\n");
        }
    }

    fn reload_selection() {
        if current_phase().is_hidden() {
            return;
        }
        let busy = with_app_mut(|app| {
            if app.nav_busy {
                true
            } else {
                app.nav_busy = true;
                false
            }
        });
        if busy {
            return;
        }
        let _guard = NavBusyGuard;
        let explorer = with_app(|app| app.explorer_hwnd);
        if unsafe { !IsWindow(explorer).as_bool() } {
            dispatch(Event::FocusLost);
            return;
        }
        if unsafe { GetForegroundWindow() != explorer } {
            return;
        }
        // A cleared selection is not a new file. Leave the current preview up.
        if let Some(path) = nonempty_path(unsafe { selected_path(explorer) }) {
            dispatch(Event::SelectionChanged(path));
        }
    }

    struct NavBusyGuard;

    impl Drop for NavBusyGuard {
        fn drop(&mut self) {
            with_app_mut(|app| app.nav_busy = false);
        }
    }

    fn on_watch_timer() {
        if current_phase().is_hidden() {
            let hwnd = main_hwnd();
            unsafe {
                let _ = KillTimer(hwnd, WATCH_TIMER_ID);
            }
            return;
        }
        if !foreground_allows_preview() {
            dispatch(Event::FocusLost);
        }
    }

    fn on_foreground_hwnd(hwnd: HWND) {
        if current_phase().is_hidden() || foreground_hwnd_allowed(hwnd) {
            return;
        }
        dispatch(Event::FocusLost);
    }

    fn foreground_hwnd_allowed(hwnd: HWND) -> bool {
        let (explorer, preview, message) =
            with_app(|app| (app.explorer_hwnd, app.preview_hwnd, app.msg_hwnd));
        hwnd == explorer || hwnd == preview || hwnd == message
    }

    fn foreground_allows_preview() -> bool {
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.0.is_null() {
            return false;
        }
        let root = unsafe { GetAncestor(foreground, GA_ROOT) };
        let foreground = if root.0.is_null() { foreground } else { root };
        let explorer = with_app(|app| app.explorer_hwnd);
        if unsafe { !IsWindow(explorer).as_bool() } {
            return false;
        }
        foreground_hwnd_allowed(foreground)
    }

    fn dismiss_queued() -> bool {
        let baseline = with_app(|app| app.input_baseline);
        INPUT_GEN.load(Ordering::Acquire) != baseline
    }

    fn origin_ready_to_show() -> bool {
        let explorer = with_app(|app| app.explorer_hwnd);
        let alive = unsafe { IsWindow(explorer).as_bool() };
        let foreground = unsafe { GetForegroundWindow() == explorer };
        gate_show(alive, foreground, false) == ShowGate::Show
    }

    fn dispatch(event: Event) {
        let effects = with_app_mut(|app| {
            let state = std::mem::replace(&mut app.state, PreviewState::Hidden);
            let (next, effects) = step(state, event, &app.cancel);
            app.state = next;
            publish_session(&app.state, app.explorer_hwnd);
            effects
        });
        sync_watch_timer();
        for effect in effects {
            apply(effect);
        }
    }

    fn apply(effect: Effect) {
        match effect {
            Effect::StartLoad { gen, path } => start_load(gen, path),
            Effect::PresentPreview => present_preview(),
            Effect::UpdateContent => update_content(),
            Effect::HideWindow { restore_focus } => hide_preview(restore_focus),
            Effect::CancelLoad => {
                with_app(|app| {
                    let _ = app.cancel.generate();
                });
            }
        }
    }

    fn start_load(gen: Generation, path: PathBuf) {
        let baseline = INPUT_GEN.load(Ordering::Acquire);
        let (cancel, hwnd) = with_app_mut(|app| {
            app.input_baseline = baseline;
            (app.cancel.clone(), app.msg_hwnd)
        });
        if path.as_os_str().is_empty() {
            return;
        }
        let hwnd_bits = hwnd.0 as isize;
        let spawned = std::thread::Builder::new()
            .name("spacepeek-load".to_string())
            .spawn(move || {
                let result = load_text_preview(&path, PreviewLimits::default(), &cancel, gen);
                if cancel.is_cancelled(&gen) {
                    return;
                }
                let result = enrich_shortcut(&path, result);
                if cancel.is_cancelled(&gen) {
                    return;
                }
                let result = enrich_quick_info(&path, result, &cancel, gen);
                if cancel.is_cancelled(&gen) {
                    return;
                }
                post_load(HWND(hwnd_bits as *mut _), gen, result);
            });
        if spawned.is_err() {
            post_load(
                hwnd,
                gen,
                Err(PreviewError::Io(std::io::Error::other(
                    "could not start preview load",
                ))),
            );
        }
    }

    fn post_load(hwnd: HWND, gen: Generation, result: std::result::Result<Preview, PreviewError>) {
        if hwnd.0.is_null() {
            return;
        }
        let raw = Box::into_raw(Box::new(LoadMsg { gen, result }));
        let posted =
            unsafe { PostMessageW(hwnd, WM_SPACEPEEK_LOADED, WPARAM(0), LPARAM(raw as isize)) };
        if posted.is_err() {
            drop(unsafe { Box::from_raw(raw) });
        }
    }

    /// Fill in a `.lnk` target when the loader classified the file as a shortcut.
    ///
    /// The load thread is not the UI STA, so this initializes COM for itself and
    /// releases it. `IShellLinkW::GetPath` reads the stored path only; `Resolve`
    /// is not called, because that can search and show UI.
    fn enrich_shortcut(
        path: &Path,
        result: std::result::Result<Preview, PreviewError>,
    ) -> std::result::Result<Preview, PreviewError> {
        match result {
            Err(PreviewError::Shortcut { .. }) => Err(PreviewError::Shortcut {
                target: read_shortcut_target(path),
            }),
            other => other,
        }
    }

    /// Type, size, and modified time for "not supported" and "not text".
    ///
    /// Runs on the load thread. `metadata` and `SHGetFileInfoW` with
    /// `SHGFI_USEFILEATTRIBUTES` do not read file contents. A cancel that lands
    /// during the lookup drops the result instead of posting it.
    fn enrich_quick_info(
        path: &Path,
        result: std::result::Result<Preview, PreviewError>,
        cancel: &CancelToken,
        gen: Generation,
    ) -> std::result::Result<Preview, PreviewError> {
        let Err(err) = result else {
            return result;
        };
        if !err.wants_quick_info() {
            return Err(err);
        }
        if cancel.is_cancelled(&gen) {
            return Err(PreviewError::Cancelled);
        }
        let info = gather_quick_info(path, cancel, gen);
        if cancel.is_cancelled(&gen) {
            return Err(PreviewError::Cancelled);
        }
        Err(err.with_quick_info(info))
    }

    fn gather_quick_info(path: &Path, cancel: &CancelToken, gen: Generation) -> QuickInfo {
        let mut info = QuickInfo {
            type_name: path
                .extension()
                .and_then(|ext| ext.to_str())
                .and_then(fallback_type_name),
            ..QuickInfo::default()
        };
        if cancel.is_cancelled(&gen) {
            return info;
        }
        if let Ok(meta) = std::fs::metadata(path) {
            info.size_bytes = Some(meta.len());
            if let Ok(modified) = meta.modified() {
                info.modified = format_local_modified(modified);
            }
        }
        if cancel.is_cancelled(&gen) {
            return info;
        }
        if let Some(name) = shell_type_name(path) {
            info.type_name = Some(name);
        }
        info
    }

    /// Windows type name from the extension. `SHGFI_USEFILEATTRIBUTES` keeps the
    /// shell from opening the file or running a shell extension against it.
    fn shell_type_name(path: &Path) -> Option<String> {
        let wide = wide_path(path);
        let mut info = SHFILEINFOW::default();
        let flags = SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES;
        let got = unsafe {
            SHGetFileInfoW(
                PCWSTR(wide.as_ptr()),
                FILE_ATTRIBUTE_NORMAL,
                Some(&mut info),
                std::mem::size_of::<SHFILEINFOW>() as u32,
                flags,
            )
        };
        if got == 0 {
            return None;
        }
        wide_to_text(&info.szTypeName)
    }

    fn format_local_modified(modified: std::time::SystemTime) -> Option<String> {
        let file_time = system_time_to_filetime(modified)?;
        let mut utc = SYSTEMTIME::default();
        unsafe { FileTimeToSystemTime(&file_time, &mut utc).ok()? };
        let mut local = SYSTEMTIME::default();
        unsafe { SystemTimeToTzSpecificLocalTime(None, &utc, &mut local).ok()? };
        let date = format_locale_date(&local);
        let time = format_locale_time(&local);
        match (date, time) {
            (Some(date), Some(time)) => Some(format!("{date} {time}")),
            (Some(date), None) => Some(date),
            (None, Some(time)) => Some(time),
            (None, None) => None,
        }
    }

    fn format_locale_date(stamp: &SYSTEMTIME) -> Option<String> {
        let mut buf = [0u16; 128];
        let n = unsafe {
            GetDateFormatEx(
                PCWSTR::null(),
                DATE_LONGDATE,
                Some(stamp),
                PCWSTR::null(),
                Some(&mut buf),
                PCWSTR::null(),
            )
        };
        locale_text(&buf, n)
    }

    fn format_locale_time(stamp: &SYSTEMTIME) -> Option<String> {
        let mut buf = [0u16; 128];
        let n = unsafe {
            GetTimeFormatEx(
                PCWSTR::null(),
                TIME_NOSECONDS,
                Some(stamp),
                PCWSTR::null(),
                Some(&mut buf),
            )
        };
        locale_text(&buf, n)
    }

    fn locale_text(buf: &[u16], written: i32) -> Option<String> {
        if written <= 1 {
            return None;
        }
        let len = (written as usize).saturating_sub(1).min(buf.len());
        wide_to_text(&buf[..len])
    }

    /// 100-nanosecond ticks since 1601-01-01 UTC. `None` before the Unix epoch.
    fn system_time_to_filetime(time: std::time::SystemTime) -> Option<FILETIME> {
        const HUNDRED_NS_TO_UNIX: u128 = 116_444_736_000_000_000;
        let since_unix = time.duration_since(std::time::UNIX_EPOCH).ok()?;
        let ticks = since_unix.as_nanos() / 100 + HUNDRED_NS_TO_UNIX;
        let ticks = u64::try_from(ticks).ok()?;
        Some(FILETIME {
            dwLowDateTime: ticks as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        })
    }

    fn read_shortcut_target(path: &Path) -> Option<String> {
        let _com = ComInit::enter()?;
        unsafe { shortcut_target(path) }
    }

    struct ComInit;

    impl ComInit {
        fn enter() -> Option<Self> {
            match unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok() } {
                Ok(()) => Some(Self),
                Err(_) => None,
            }
        }
    }

    impl Drop for ComInit {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    unsafe fn shortcut_target(path: &Path) -> Option<String> {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        let file: IPersistFile = link.cast().ok()?;
        let wide = wide_path(path);
        file.Load(PCWSTR(wide.as_ptr()), STGM_READ).ok()?;
        let mut buf = vec![0u16; 32 * 1024];
        let mut data = WIN32_FIND_DATAW::default();
        let flags = u32::try_from(SLGP_RAWPATH.0).unwrap_or(0);
        link.GetPath(&mut buf, std::ptr::from_mut(&mut data), flags)
            .ok()?;
        wide_to_text(&buf)
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_to_text(buf: &[u16]) -> Option<String> {
        let end = buf.iter().position(|unit| *unit == 0).unwrap_or(buf.len());
        let text = String::from_utf16_lossy(&buf[..end]);
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn present_preview() {
        if !is_pending_show() || dismiss_queued() {
            return;
        }
        if !origin_ready_to_show() {
            debug_log("Spacepeek: preview discarded before show\n");
            dispatch(Event::ShowAborted);
            return;
        }
        // Filling the edit can take long enough for Esc or a focus change to
        // land in the queue. Check again before the window becomes visible.
        if !fill_preview_window() {
            dispatch(Event::ShowAborted);
            return;
        }
        if !is_pending_show() || dismiss_queued() {
            return;
        }
        if !origin_ready_to_show() {
            debug_log("Spacepeek: preview discarded before show\n");
            dispatch(Event::ShowAborted);
            return;
        }
        reveal_preview();
        // ShowWindow itself can overlap a key or a focus change. Roll the
        // pixels back. A queued Space or Esc must stay PendingShow: aborting
        // to Hidden would let that Space start a second preview.
        if !is_pending_show() {
            return;
        }
        if dismiss_queued() {
            hide_preview(false);
            return;
        }
        if !origin_ready_to_show() {
            debug_log("Spacepeek: preview discarded before show\n");
            dispatch(Event::ShowAborted);
            return;
        }
        dispatch(Event::ShowCommitted);
    }

    fn update_content() {
        if current_phase() != SessionPhase::Visible {
            return;
        }
        if !foreground_allows_preview() {
            dispatch(Event::FocusLost);
            return;
        }
        if dismiss_queued() {
            return;
        }
        if !fill_preview_window() {
            dispatch(Event::FocusLost);
            return;
        }
        if !foreground_allows_preview() {
            dispatch(Event::FocusLost);
            return;
        }
        reveal_preview();
    }

    fn is_pending_show() -> bool {
        with_app(|app| matches!(app.state, PreviewState::PendingShow { .. }))
    }

    /// Create the preview hidden, or replace the text of the existing one.
    /// Does not activate or show the window. Returns false if the window
    /// could not be created. An existing window keeps its pixel size; only a
    /// new window takes the remembered (or default) outer size.
    fn fill_preview_window() -> bool {
        let (presented, anchor, instance, existing) = with_app(|app| {
            (
                presented_from(&app.state),
                app.explorer_hwnd,
                app.instance,
                app.preview_hwnd,
            )
        });
        let Some(presented) = presented else {
            return false;
        };
        let dpi_anchor = if existing.0.is_null() {
            anchor
        } else {
            existing
        };
        let dpi = dpi_for_anchor(dpi_anchor);
        let (fonts, stale) = swap_fonts(dpi);
        let _stale = stale;
        let title = presented.header().name.clone();

        if existing.0.is_null() {
            let Some(hwnd) = create_preview_window(anchor, instance, dpi, &presented) else {
                return false;
            };
            // Publish before the edit is created so parent messages, including
            // a size from the child, hit `preview_proc`.
            with_app_mut(|app| {
                app.preview_hwnd = hwnd;
                app.edit_hwnd = HWND::default();
                app.presented = Some(paint_presented(&presented));
                publish_preview_hwnd(hwnd);
            });
            if !install_body(hwnd, instance, &presented, fonts.edit) {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                with_app_mut(|app| {
                    if app.preview_hwnd == hwnd {
                        app.preview_hwnd = HWND::default();
                        app.edit_hwnd = HWND::default();
                        app.presented = None;
                        publish_preview_hwnd(HWND::default());
                    }
                });
                return false;
            }
            set_text(hwnd, &title);
            invalidate_preview(hwnd);
            remember_shown_path();
            true
        } else {
            if !install_body(existing, instance, &presented, fonts.edit) {
                return false;
            }
            set_text(existing, &title);
            with_app_mut(|app| {
                app.presented = Some(paint_presented(&presented));
            });
            publish_preview_hwnd(existing);
            invalidate_preview(existing);
            remember_shown_path();
            true
        }
    }

    /// Path of the file whose contents are on screen. A reload that has not
    /// finished still names the previous file.
    fn remember_shown_path() {
        let path = with_app(|app| match &app.state {
            PreviewState::Showing { path, .. }
            | PreviewState::Failed { path, .. }
            | PreviewState::PendingShow { path, .. } => Some(path.clone()),
            _ => None,
        });
        if let Some(path) = path {
            with_app_mut(|app| app.shown_path = path);
        }
    }

    /// Close through the state machine, then open `shown_path` with the shell
    /// default verb. The path is taken first so a triple-click cannot open twice.
    /// A failed open leaves the preview closed and shows no error UI.
    fn open_current_file() {
        let path = with_app_mut(|app| {
            if app.shown_path.as_os_str().is_empty() {
                None
            } else {
                Some(std::mem::take(&mut app.shown_path))
            }
        });
        let Some(path) = path else {
            return;
        };
        dispatch(Event::EscPressed);
        shell_open(&path);
    }

    fn shell_open(path: &Path) {
        let wide = wide_path(path);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_FLAG_NO_UI,
            lpFile: PCWSTR(wide.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        let _ = unsafe { ShellExecuteExW(&mut info) };
    }

    fn presented_from(state: &PreviewState) -> Option<Presented> {
        match state {
            PreviewState::Showing { path, preview, .. } => Some(chrome_for_text(path, preview)),
            PreviewState::Failed { path, error, .. } => Some(chrome_for_error(path, error)),
            PreviewState::PendingShow { path, outcome, .. } => match outcome {
                ShowOutcome::Ready(preview) => Some(chrome_for_text(path, preview)),
                ShowOutcome::Failed(error) => Some(chrome_for_error(path, error)),
            },
            _ => None,
        }
    }

    /// Header and message text for painting. The edit control owns file text,
    /// so that copy is not kept a second time.
    fn paint_presented(presented: &Presented) -> Presented {
        match presented {
            Presented::Text { header, .. } => Presented::Text {
                header: header.clone(),
                body: String::new(),
            },
            Presented::Message { header, message } => Presented::Message {
                header: header.clone(),
                message: message.clone(),
            },
            // Step 2 paints the bitmap. Keep it on the presented value so the
            // header and the pixels stay together; the body has no edit control.
            Presented::Image { header, image } => Presented::Image {
                header: header.clone(),
                image: std::sync::Arc::clone(image),
            },
        }
    }

    fn create_preview_window(
        anchor: HWND,
        instance: HINSTANCE,
        dpi: u32,
        presented: &Presented,
    ) -> Option<HWND> {
        let size = with_app(|app| app.sizes.opening_physical(presented.kind(), dpi));
        let placed = place_preview(
            anchor_rect(anchor),
            monitor_work_area(anchor),
            size.width,
            size.height,
        );
        let class_name = utf16(PREVIEW_CLASS);
        let title = utf16(&presented.header().name);
        unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP | WS_THICKFRAME | WS_CLIPCHILDREN,
                placed.x,
                placed.y,
                placed.width,
                placed.height,
                HWND::default(),
                HMENU::default(),
                instance,
                None,
            )
            .ok()
        }
    }

    fn install_body(
        hwnd: HWND,
        instance: HINSTANCE,
        presented: &Presented,
        edit_font: HFONT,
    ) -> bool {
        match presented {
            Presented::Text { body, .. } => {
                let Some(edit) = ensure_edit(hwnd, instance) else {
                    return false;
                };
                apply_font(edit, edit_font);
                set_text(edit, body);
                resize_edit_pair(hwnd, edit);
                suppress_caret(edit);
                true
            }
            Presented::Message { .. } | Presented::Image { .. } => {
                destroy_edit();
                true
            }
        }
    }

    fn ensure_edit(parent: HWND, instance: HINSTANCE) -> Option<HWND> {
        let existing = with_app(|app| app.edit_hwnd);
        if !existing.0.is_null() && unsafe { IsWindow(existing).as_bool() } {
            return Some(existing);
        }
        let edit_class = utf16("EDIT");
        // ES_AUTOHSCROLL turns word wrap off for a multiline EDIT. WS_HSCROLL
        // keeps a bar so columns and indentation scroll sideways instead of
        // breaking mid-token.
        let edit_style = WS_CHILD
            | WS_VISIBLE
            | WS_VSCROLL
            | WS_HSCROLL
            | WINDOW_STYLE(ES_MULTILINE as u32)
            | WINDOW_STYLE(ES_READONLY as u32)
            | WINDOW_STYLE(ES_AUTOVSCROLL as u32)
            | WINDOW_STYLE(ES_AUTOHSCROLL as u32);
        let edit = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(edit_class.as_ptr()),
                PCWSTR::null(),
                edit_style,
                0,
                0,
                0,
                0,
                parent,
                HMENU::default(),
                instance,
                None,
            )
            .ok()?
        };
        unsafe {
            let _ = SendMessageW(edit, EM_SETLIMITTEXT, WPARAM(1_048_576), LPARAM(0));
        }
        subclass_edit(edit);
        with_app_mut(|app| app.edit_hwnd = edit);
        Some(edit)
    }

    fn destroy_edit() {
        let edit = with_app_mut(|app| std::mem::take(&mut app.edit_hwnd));
        if edit.0.is_null() {
            return;
        }
        unsafe {
            let _ = DestroyWindow(edit);
        }
        EDIT_PROC.store(0, Ordering::Release);
    }

    fn reveal_preview() {
        let hwnd = with_app(|app| app.preview_hwnd);
        if hwnd.0.is_null() {
            return;
        }
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
        }
        suppress_caret(with_app(|app| app.edit_hwnd));
    }

    fn hide_preview(restore_focus: bool) {
        let (preview, explorer) = with_app(|app| (app.preview_hwnd, app.explorer_hwnd));
        with_app_mut(|app| app.last_nav_tick = None);
        let hwnd = main_hwnd();
        unsafe {
            let _ = KillTimer(hwnd, NAV_TIMER_ID);
        }
        if preview.0.is_null() {
            if restore_focus {
                restore_explorer_focus(explorer);
            }
            return;
        }
        unsafe {
            let _ = ShowWindow(preview, SW_HIDE);
            let _ = DestroyWindow(preview);
        }
        with_app_mut(|app| {
            app.preview_hwnd = HWND::default();
            app.edit_hwnd = HWND::default();
            app.presented = None;
            app.close_hovered = false;
            app.shown_path.clear();
            publish_preview_hwnd(HWND::default());
        });
        EDIT_PROC.store(0, Ordering::Release);
        if restore_focus {
            restore_explorer_focus(explorer);
        }
    }

    fn restore_explorer_focus(explorer: HWND) {
        unsafe {
            if !IsWindow(explorer).as_bool() || GetForegroundWindow() == explorer {
                return;
            }
        }
        force_foreground(explorer);
    }

    fn scroll_preview(op: ScrollOp) {
        let edit = with_app(|app| app.edit_hwnd);
        if edit.0.is_null() {
            return;
        }
        let command = match op {
            ScrollOp::PageUp => SB_PAGEUP,
            ScrollOp::PageDown => SB_PAGEDOWN,
            ScrollOp::Top => SB_TOP,
            ScrollOp::Bottom => SB_BOTTOM,
        };
        unsafe {
            let _ = SendMessageW(edit, WM_VSCROLL, WPARAM(command.0 as usize), LPARAM(0));
        }
        suppress_caret(edit);
    }

    fn scroll_wheel(delta: i32) {
        let edit = with_app(|app| app.edit_hwnd);
        if edit.0.is_null() {
            return;
        }
        let dy = wheel_scroll_dy(delta, WHEEL_LINES_PER_NOTCH);
        if dy == 0 {
            return;
        }
        unsafe {
            let _ = SendMessageW(edit, EM_LINESCROLL, WPARAM(0), LPARAM(dy as isize));
        }
        suppress_caret(edit);
    }

    fn suppress_caret(edit: HWND) {
        if edit.0.is_null() {
            return;
        }
        unsafe {
            let _ = HideCaret(edit);
        }
    }

    fn subclass_edit(edit: HWND) {
        unsafe {
            let previous = SetWindowLongPtrW(edit, GWLP_WNDPROC, edit_proc as *const () as isize);
            EDIT_PROC.store(previous, Ordering::Release);
        }
    }

    unsafe extern "system" fn edit_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        // Client clicks would focus the edit and show a caret. Scrollbar clicks
        // are non-client and still reach the original procedure.
        match msg {
            // The edit covers only the body. A double-click opens the file
            // instead of selecting a word. Posted so this procedure returns
            // before the preview window is destroyed. The original procedure
            // is not called, so the word-select does not run.
            WM_LBUTTONDBLCLK => {
                if double_click_action(HitZone::Client) == DoubleClickAction::Open {
                    if let Ok(parent) = GetParent(hwnd) {
                        if !parent.0.is_null() {
                            let _ = PostMessageW(parent, WM_SPACEPEEK_OPEN, WPARAM(0), LPARAM(0));
                        }
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_RBUTTONDBLCLK
            | WM_MBUTTONDOWN | WM_MBUTTONUP | WM_MBUTTONDBLCLK | WM_KEYDOWN | WM_KEYUP
            | WM_CHAR | WM_SYSKEYDOWN | WM_SYSKEYUP => LRESULT(0),
            WM_SETFOCUS => {
                let result = call_edit_proc(hwnd, msg, wparam, lparam);
                let _ = HideCaret(hwnd);
                result
            }
            _ => call_edit_proc(hwnd, msg, wparam, lparam),
        }
    }

    fn call_edit_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let bits = EDIT_PROC.load(Ordering::Acquire);
        if bits == 0 {
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        type EditFn = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;
        let previous = unsafe { std::mem::transmute::<isize, EditFn>(bits) };
        unsafe { CallWindowProcW(Some(previous), hwnd, msg, wparam, lparam) }
    }

    fn resize_edit(hwnd: HWND) {
        let edit = with_app(|app| app.edit_hwnd);
        if !edit.0.is_null() {
            resize_edit_pair(hwnd, edit);
        }
    }

    fn resize_edit_pair(parent: HWND, edit: HWND) {
        let mut rc = RECT::default();
        unsafe {
            if GetClientRect(parent, &mut rc).is_ok() {
                let dpi = dpi_for_anchor(parent);
                let header = scaled_header_height(dpi);
                let width = (rc.right - rc.left).max(0);
                let height = (rc.bottom - rc.top - header).max(0);
                let _ = MoveWindow(edit, 0, header, width, height, true);
            }
        }
    }

    fn anchor_rect(anchor: HWND) -> Option<PxRect> {
        if anchor.0.is_null() {
            return None;
        }
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(anchor, &mut rect).is_err() } {
            return None;
        }
        let parsed = PxRect {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        };
        if parsed.right > parsed.left && parsed.bottom > parsed.top {
            Some(parsed)
        } else {
            None
        }
    }

    fn monitor_work_area(anchor: HWND) -> PxRect {
        unsafe {
            let monitor = MonitorFromWindow(anchor, MONITOR_DEFAULTTONEAREST);
            if !monitor.is_invalid() {
                let mut info = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..MONITORINFO::default()
                };
                if GetMonitorInfoW(monitor, &mut info).as_bool() {
                    return PxRect {
                        left: info.rcWork.left,
                        top: info.rcWork.top,
                        right: info.rcWork.right,
                        bottom: info.rcWork.bottom,
                    };
                }
            }
            // Only when the monitor query itself fails. Centring uses rcWork.
            PxRect {
                left: 0,
                top: 0,
                right: GetSystemMetrics(SM_CXSCREEN).max(0),
                bottom: GetSystemMetrics(SM_CYSCREEN).max(0),
            }
        }
    }

    fn dpi_for_anchor(anchor: HWND) -> u32 {
        effective_dpi(unsafe { query_dpi(anchor) })
    }

    unsafe fn query_dpi(anchor: HWND) -> u32 {
        if !anchor.0.is_null() {
            let dpi = GetDpiForWindow(anchor);
            if dpi != 0 {
                return dpi;
            }
        }
        let monitor = MonitorFromWindow(anchor, MONITOR_DEFAULTTONEAREST);
        if monitor.is_invalid() {
            return 0;
        }
        let mut x = 0u32;
        let mut y = 0u32;
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y).is_ok() && x != 0 {
            x
        } else {
            0
        }
    }

    fn on_preview_dpi_changed(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
        if lparam.0 != 0 {
            let suggested = unsafe { *(lparam.0 as *const RECT) };
            let width = suggested.right - suggested.left;
            let height = suggested.bottom - suggested.top;
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        let dpi = (wparam.0 as u32) & 0xffff;
        let (fonts, stale) = swap_fonts(dpi);
        let _stale = stale;
        apply_font(with_app(|app| app.edit_hwnd), fonts.edit);
        resize_edit(hwnd);
        invalidate_preview(hwnd);
    }

    fn zone_to_hit(zone: HitZone) -> u32 {
        match zone {
            HitZone::Caption => HTCAPTION,
            // The × is client area. `HTCLOSE` would let DefWindowProc post
            // `WM_CLOSE` as well as the click we handle ourselves.
            HitZone::Close | HitZone::Client => HTCLIENT,
            HitZone::Left => HTLEFT,
            HitZone::Right => HTRIGHT,
            HitZone::Top => HTTOP,
            HitZone::TopLeft => HTTOPLEFT,
            HitZone::TopRight => HTTOPRIGHT,
            HitZone::Bottom => HTBOTTOM,
            HitZone::BottomLeft => HTBOTTOMLEFT,
            HitZone::BottomRight => HTBOTTOMRIGHT,
        }
    }

    fn hit_at_screen(hwnd: HWND, lparam: LPARAM) -> HitZone {
        let (x, y) = screen_point_in_window(hwnd, lparam);
        hit_test(window_layout(hwnd), x, y)
    }

    fn hit_at_client(hwnd: HWND, lparam: LPARAM) -> HitZone {
        let (x, y) = client_point_in_window(hwnd, lparam);
        hit_test(window_layout(hwnd), x, y)
    }

    /// `WM_NCHITTEST` / `WM_NCMOUSEMOVE` points are screen pixels.
    fn screen_point_in_window(hwnd: HWND, lparam: LPARAM) -> (i32, i32) {
        let (x, y) = lparam_xy(lparam.0 as usize);
        let mut window = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut window).is_err() } {
            return (0, 0);
        }
        (x - window.left, y - window.top)
    }

    /// Client mouse messages are relative to the client origin. [`hit_test`]
    /// and [`pointer_over_close`] want outer-window pixels.
    fn client_point_in_window(hwnd: HWND, lparam: LPARAM) -> (i32, i32) {
        let (x, y) = lparam_xy(lparam.0 as usize);
        let layout = window_layout(hwnd);
        (x + layout.frame_left, y + layout.frame_top)
    }

    fn update_close_hover(hwnd: HWND, hovered: bool) {
        let changed = with_app_mut(|app| {
            if app.close_hovered == hovered {
                false
            } else {
                app.close_hovered = hovered;
                true
            }
        });
        if changed {
            invalidate_close_button(hwnd);
        }
    }

    fn track_client_leave(hwnd: HWND) {
        let mut event = TRACKMOUSEEVENT {
            cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: TME_LEAVE,
            hwndTrack: hwnd,
            dwHoverTime: 0,
        };
        let _ = unsafe { TrackMouseEvent(&mut event) };
    }

    fn invalidate_close_button(hwnd: HWND) {
        let Some(rect) = close_button_client_rect(hwnd) else {
            return;
        };
        unsafe {
            let _ = InvalidateRect(hwnd, Some(&rect), false);
        }
    }

    fn close_button_client_rect(hwnd: HWND) -> Option<RECT> {
        let mut client = RECT::default();
        if unsafe { GetClientRect(hwnd, &mut client).is_err() } {
            return None;
        }
        let width = client.right.saturating_sub(client.left);
        let height = client.bottom.saturating_sub(client.top);
        let dpi = dpi_for_anchor(hwnd);
        let header = scaled_header_height(dpi).clamp(0, height);
        let close = scaled_close_width(dpi);
        let bounds = close_button_bounds(width, header, close)?;
        Some(RECT {
            left: bounds.left,
            top: bounds.top,
            right: bounds.right,
            bottom: bounds.bottom,
        })
    }

    fn window_layout(hwnd: HWND) -> ChromeLayout {
        let dpi = dpi_for_anchor(hwnd);
        let header_height = scaled_header_height(dpi);
        let close_width = scaled_close_width(dpi);
        let mut window = RECT::default();
        let mut client = RECT::default();
        let measured = unsafe {
            GetWindowRect(hwnd, &mut window).is_ok() && GetClientRect(hwnd, &mut client).is_ok()
        };
        if measured {
            let mut origin = POINT {
                x: client.left,
                y: client.top,
            };
            if unsafe { ClientToScreen(hwnd, &mut origin).as_bool() } {
                let width = window.right.saturating_sub(window.left);
                let height = window.bottom.saturating_sub(window.top);
                let client_w = client.right.saturating_sub(client.left);
                let client_h = client.bottom.saturating_sub(client.top);
                let mut frame_left = origin.x.saturating_sub(window.left);
                let mut frame_top = origin.y.saturating_sub(window.top);
                let mut frame_right = window
                    .right
                    .saturating_sub(origin.x)
                    .saturating_sub(client_w);
                let mut frame_bottom = window
                    .bottom
                    .saturating_sub(origin.y)
                    .saturating_sub(client_h);
                if frame_left == 0 && frame_top == 0 && frame_right == 0 && frame_bottom == 0 {
                    let frame = frame_thickness(dpi);
                    frame_left = frame;
                    frame_top = frame;
                    frame_right = frame;
                    frame_bottom = frame;
                }
                return ChromeLayout {
                    width,
                    height,
                    frame_left,
                    frame_top,
                    frame_right,
                    frame_bottom,
                    header_height,
                    close_width,
                };
            }
        }
        ChromeLayout {
            width: 0,
            height: 0,
            frame_left: 0,
            frame_top: 0,
            frame_right: 0,
            frame_bottom: 0,
            header_height,
            close_width,
        }
    }

    fn frame_thickness(dpi: u32) -> i32 {
        let dpi = effective_dpi(dpi);
        let frame = unsafe { GetSystemMetricsForDpi(SM_CXFRAME, dpi) };
        let pad = unsafe { GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) };
        let sum = frame.saturating_add(pad);
        if sum > 0 {
            sum
        } else {
            spacepeek::geometry::scale_for_dpi(8, dpi).max(1)
        }
    }

    fn apply_min_track(hwnd: HWND, lparam: LPARAM) {
        if lparam.0 == 0 {
            return;
        }
        let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
        let dpi = dpi_for_anchor(hwnd);
        let min = min_physical_size(dpi);
        info.ptMinTrackSize.x = min.width;
        info.ptMinTrackSize.y = min.height;
    }

    /// Store the outer size after the user finishes a move or resize.
    ///
    /// A drag that leaves the size unchanged does not replace the remembered
    /// value. DPI moves are `WM_DPICHANGED`, not this path, so the stored
    /// size stays in logical pixels.
    fn remember_window_size(hwnd: HWND) {
        let mut rect = RECT::default();
        if unsafe { GetWindowRect(hwnd, &mut rect).is_err() } {
            return;
        }
        let width = rect.right.saturating_sub(rect.left);
        let height = rect.bottom.saturating_sub(rect.top);
        if width <= 0 || height <= 0 {
            return;
        }
        let dpi = dpi_for_anchor(hwnd);
        with_app_mut(|app| {
            let kind = app
                .presented
                .as_ref()
                .map(Presented::kind)
                .unwrap_or(WindowKind::Content);
            let clamped = spacepeek::chrome::clamp_logical_size(
                spacepeek::chrome::physical_to_logical(width, height, dpi),
            );
            if app.sizes.chosen() == Some(clamped) {
                return;
            }
            if app.sizes.chosen().is_none()
                && clamped == spacepeek::chrome::default_logical_size(kind)
            {
                return;
            }
            let _ = app.sizes.remember(clamped);
        });
    }

    fn invalidate_preview(hwnd: HWND) {
        if hwnd.0.is_null() {
            return;
        }
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
    }

    struct PaintSnap {
        name: String,
        details: String,
        message: Option<String>,
        image: Option<std::sync::Arc<DecodedImage>>,
        header_font: HFONT,
        detail_font: HFONT,
        close_hovered: bool,
    }

    fn paint_preview(hwnd: HWND) {
        let snap = with_app(|app| {
            app.presented.as_ref().map(|presented| PaintSnap {
                name: presented.header().name.clone(),
                details: presented.header().details.clone(),
                message: presented.message().map(str::to_owned),
                image: match presented {
                    Presented::Image { image, .. } => Some(std::sync::Arc::clone(image)),
                    _ => None,
                },
                header_font: app.header_font,
                detail_font: app.detail_font,
                close_hovered: app.close_hovered,
            })
        });
        let mut ps = PAINTSTRUCT::default();
        let dc = unsafe { BeginPaint(hwnd, &mut ps) };
        if !dc.is_invalid() {
            if let Some(snap) = snap.as_ref() {
                paint_chrome(hwnd, dc, snap);
            }
        }
        unsafe {
            let _ = EndPaint(hwnd, &ps);
        }
    }

    fn paint_chrome(hwnd: HWND, dc: HDC, snap: &PaintSnap) {
        let mut client = RECT::default();
        if unsafe { GetClientRect(hwnd, &mut client).is_err() } {
            return;
        }
        let width = client.right - client.left;
        let height = client.bottom - client.top;
        if width <= 0 || height <= 0 {
            return;
        }
        let dpi = dpi_for_anchor(hwnd);
        let header_h = scaled_header_height(dpi).clamp(0, height);
        let close_w = scaled_close_width(dpi).clamp(0, width);
        let pad = scaled_header_pad(dpi).max(0);
        let body = RECT {
            left: 0,
            top: header_h,
            right: width,
            bottom: height,
        };
        fill_rect(
            dc,
            &RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: header_h,
            },
            rgb(243, 243, 243),
        );
        // The image fills its own rect after the header. Skipping the body
        // here lets that path exclude the bitmap and avoid a white flash.
        if snap.image.is_none() {
            fill_rect(dc, &body, rgb(255, 255, 255));
        }
        let text_right = (width - close_w).max(pad);
        if snap.close_hovered {
            paint_close_hover(
                dc,
                RECT {
                    left: width - close_w,
                    top: 0,
                    right: width,
                    bottom: header_h,
                },
                dpi,
            );
        }
        let mid = header_h / 2;
        let single = DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX;
        draw_text(
            dc,
            &snap.name,
            &mut RECT {
                left: pad,
                top: 0,
                right: text_right,
                bottom: mid,
            },
            single,
            snap.header_font,
            rgb(32, 32, 32),
        );
        draw_text(
            dc,
            &snap.details,
            &mut RECT {
                left: pad,
                top: mid,
                right: text_right,
                bottom: header_h.saturating_sub(1),
            },
            single,
            snap.detail_font,
            rgb(96, 96, 96),
        );
        draw_text(
            dc,
            "\u{00D7}",
            &mut RECT {
                left: width - close_w,
                top: 0,
                right: width,
                bottom: header_h,
            },
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            snap.header_font,
            rgb(32, 32, 32),
        );
        let sep = spacepeek::geometry::scale_for_dpi(1, dpi).max(1);
        fill_rect(
            dc,
            &RECT {
                left: 0,
                top: header_h.saturating_sub(sep),
                right: width,
                bottom: header_h,
            },
            rgb(210, 210, 210),
        );
        if let Some(image) = snap.image.as_deref() {
            paint_image(dc, image, &body, dpi);
        } else if let Some(message) = snap.message.as_deref() {
            let left = pad.min(width / 4);
            let right = (width - pad).max(left);
            paint_message(
                dc,
                message,
                RECT {
                    left,
                    top: header_h,
                    right,
                    bottom: height,
                },
                snap.header_font,
            );
        }
    }

    /// Draw `image` centred in `body`. The background is filled around the
    /// destination so a resize does not wipe the bitmap before the stretch.
    fn paint_image(dc: HDC, image: &DecodedImage, body: &RECT, dpi: u32) {
        let area = PxRect {
            left: body.left,
            top: body.top,
            right: body.right,
            bottom: body.bottom,
        };
        let dest = image_dest_rect(
            area,
            image.width,
            image.height,
            image.source_width,
            image.source_height,
            dpi,
        );
        let dest_w = dest.width();
        let dest_h = dest.height();
        let src_w = i32::try_from(image.width).unwrap_or(0);
        let src_h = i32::try_from(image.height).unwrap_or(0);
        let pixels = u64::from(image.width)
            .saturating_mul(u64::from(image.height))
            .saturating_mul(4);
        let drawable = dest_w > 0
            && dest_h > 0
            && src_w > 0
            && src_h > 0
            && pixels > 0
            && pixels <= image.bgra.len() as u64;
        if !drawable {
            fill_rect(dc, body, rgb(255, 255, 255));
            return;
        }
        let saved = unsafe { SaveDC(dc) };
        if saved != 0 {
            let _ = unsafe { ExcludeClipRect(dc, dest.left, dest.top, dest.right, dest.bottom) };
        }
        fill_rect(dc, body, rgb(255, 255, 255));
        if saved != 0 {
            let _ = unsafe { RestoreDC(dc, saved) };
        }
        let mut info = BITMAPINFO::default();
        info.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        info.bmiHeader.biWidth = src_w;
        info.bmiHeader.biHeight = src_h.saturating_neg();
        info.bmiHeader.biPlanes = 1;
        info.bmiHeader.biBitCount = 32;
        info.bmiHeader.biCompression = BI_RGB.0;
        unsafe {
            let _ = SetStretchBltMode(dc, HALFTONE);
            let _ = SetBrushOrgEx(dc, 0, 0, None);
            let _ = StretchDIBits(
                dc,
                dest.left,
                dest.top,
                dest_w,
                dest_h,
                0,
                0,
                src_w,
                src_h,
                Some(image.bgra.as_ptr().cast()),
                &info,
                DIB_RGB_COLORS,
                SRCCOPY,
            );
        }
    }

    fn paint_message(dc: HDC, message: &str, area: RECT, font: HFONT) {
        let left = area.left;
        let right = area.right;
        let area_top = area.top;
        let area_h = (area.bottom - area.top).max(0);
        if right <= left || area_h <= 0 {
            return;
        }
        let mut wide: Vec<u16> = message.encode_utf16().collect();
        if wide.is_empty() {
            return;
        }
        let flags = DT_WORDBREAK | DT_NOPREFIX | DT_CENTER;
        let mut measure = RECT {
            left,
            top: area_top,
            right,
            bottom: area_top + area_h,
        };
        unsafe {
            let previous = if font.0.is_null() {
                HGDIOBJ::default()
            } else {
                SelectObject(dc, font)
            };
            let _ = SetBkMode(dc, TRANSPARENT);
            let _ = SetTextColor(dc, rgb(32, 32, 32));
            let _ = DrawTextW(dc, &mut wide, &mut measure, flags | DT_CALCRECT);
            let text_h = (measure.bottom - measure.top).max(0);
            let mut draw = if text_h >= area_h {
                RECT {
                    left,
                    top: area_top,
                    right,
                    bottom: area_top + area_h,
                }
            } else {
                let top = area_top + (area_h - text_h) / 2;
                RECT {
                    left,
                    top,
                    right,
                    bottom: top + text_h,
                }
            };
            let _ = DrawTextW(dc, &mut wide, &mut draw, flags);
            if !previous.is_invalid() {
                let _ = SelectObject(dc, previous);
            }
        }
    }

    fn draw_text(
        dc: HDC,
        text: &str,
        rect: &mut RECT,
        format: DRAW_TEXT_FORMAT,
        font: HFONT,
        color: COLORREF,
    ) {
        if text.is_empty() || rect.right <= rect.left || rect.bottom <= rect.top {
            return;
        }
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            let previous = if font.0.is_null() {
                HGDIOBJ::default()
            } else {
                SelectObject(dc, font)
            };
            let _ = SetBkMode(dc, TRANSPARENT);
            let _ = SetTextColor(dc, color);
            let _ = DrawTextW(dc, &mut wide, rect, format);
            if !previous.is_invalid() {
                let _ = SelectObject(dc, previous);
            }
        }
    }

    /// Light grey pill behind the ×. Darker than the header (`243`) and not
    /// the Windows red close button.
    fn paint_close_hover(dc: HDC, button: RECT, dpi: u32) {
        let inset = spacepeek::geometry::scale_for_dpi(4, dpi).max(1);
        let rect = RECT {
            left: button.left.saturating_add(inset),
            top: button.top.saturating_add(inset),
            right: button.right.saturating_sub(inset),
            bottom: button.bottom.saturating_sub(inset),
        };
        let radius = spacepeek::geometry::scale_for_dpi(6, dpi).max(1);
        fill_round_rect(dc, &rect, radius.saturating_mul(2), rgb(220, 220, 220));
    }

    fn fill_round_rect(dc: HDC, rect: &RECT, ellipse: i32, color: COLORREF) {
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return;
        }
        let ellipse = ellipse.max(1);
        unsafe {
            let brush = CreateSolidBrush(color);
            if brush.is_invalid() {
                return;
            }
            let pen = CreatePen(PS_SOLID, 1, color);
            if pen.is_invalid() {
                let _ = DeleteObject(brush);
                return;
            }
            let old_brush = SelectObject(dc, brush);
            let old_pen = SelectObject(dc, pen);
            let _ = RoundRect(
                dc,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                ellipse,
                ellipse,
            );
            if !old_brush.is_invalid() {
                let _ = SelectObject(dc, old_brush);
            }
            if !old_pen.is_invalid() {
                let _ = SelectObject(dc, old_pen);
            }
            let _ = DeleteObject(brush);
            let _ = DeleteObject(pen);
        }
    }

    fn fill_rect(dc: HDC, rect: &RECT, color: COLORREF) {
        if rect.right <= rect.left || rect.bottom <= rect.top {
            return;
        }
        unsafe {
            let brush = CreateSolidBrush(color);
            if brush.is_invalid() {
                return;
            }
            let _ = FillRect(dc, rect, brush);
            let _ = DeleteObject(brush);
        }
    }

    fn rgb(r: u32, g: u32, b: u32) -> COLORREF {
        COLORREF(r | (g << 8) | (b << 16))
    }

    fn force_foreground(hwnd: HWND) {
        if hwnd.0.is_null() {
            return;
        }
        // Attach to the current foreground thread, not the target. When the
        // target is our preview it lives on this thread, so attaching to it
        // never runs and the foreground lock refuses SetForegroundWindow.
        let _attach = ForegroundAttach::to_current_foreground();
        unsafe {
            let _ = BringWindowToTop(hwnd);
            SetLastError(ERROR_SUCCESS);
            if !SetForegroundWindow(hwnd).as_bool() {
                let err = GetLastError().0;
                debug_log(&format!(
                    "Spacepeek: SetForegroundWindow failed (win32 {err})\n"
                ));
            }
            let _ = SetFocus(hwnd);
        }
    }

    /// Attaches this thread to the thread that owns the current foreground
    /// window, and detaches on drop so a later failure cannot leave them joined.
    struct ForegroundAttach {
        current: u32,
        foreground: u32,
        attached: bool,
    }

    impl ForegroundAttach {
        fn to_current_foreground() -> Self {
            unsafe {
                let foreground_hwnd = GetForegroundWindow();
                let foreground = if foreground_hwnd.0.is_null() {
                    0
                } else {
                    GetWindowThreadProcessId(foreground_hwnd, None)
                };
                let current = GetCurrentThreadId();
                let attached = foreground != 0
                    && foreground != current
                    && AttachThreadInput(current, foreground, true).as_bool();
                Self {
                    current,
                    foreground,
                    attached,
                }
            }
        }
    }

    impl Drop for ForegroundAttach {
        fn drop(&mut self) {
            if self.attached {
                unsafe {
                    let _ = AttachThreadInput(self.current, self.foreground, false);
                }
            }
        }
    }

    fn debug_log(text: &str) {
        let wide = utf16(text);
        unsafe {
            OutputDebugStringW(PCWSTR(wide.as_ptr()));
        }
    }

    unsafe fn selected_path(foreground: HWND) -> Option<PathBuf> {
        if foreground.0.is_null() {
            return None;
        }
        let shell: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;
        let count = shell.Count().ok()?;
        // Windows 11 tabs share the top-level HWND, so the first `IShellWindows`
        // match can be a background tab. The active tab is the first
        // `ShellTabWindowClass` child. No such child means older Explorer.
        let active_tab = active_tab_hwnd(foreground);
        let tabs = !active_tab.0.is_null();
        let mut first_match = None;
        for index in 0..count {
            let Some(web) = web_browser_at(&shell, index) else {
                continue;
            };
            let Ok(hwnd) = web.HWND() else {
                continue;
            };
            if !same_hwnd(hwnd.0, foreground) {
                continue;
            }
            if first_match.is_none() {
                first_match = Some(web.clone());
            }
            if !tabs {
                return path_from_web(&web);
            }
            let Some(browser) = top_level_browser(&web) else {
                continue;
            };
            let Ok(tab) = browser_tab_hwnd(&browser) else {
                continue;
            };
            if tab == active_tab {
                if let Some(path) = path_from_browser(&browser) {
                    return Some(path);
                }
                return path_from_automation(&web);
            }
        }
        if let Some(web) = first_match {
            // Fallback in case IOleWindow::GetWindow does not return the ShellTabWindowClass HWND on some Explorer build.
            return path_from_web(&web);
        }
        None
    }

    fn active_tab_hwnd(foreground: HWND) -> HWND {
        unsafe {
            FindWindowExW(foreground, None, w!("ShellTabWindowClass"), None).unwrap_or_default()
        }
    }

    unsafe fn web_browser_at(shell: &IShellWindows, index: i32) -> Option<IWebBrowser2> {
        let index = VARIANT::from(index);
        let dispatch = shell.Item(&index).ok()?;
        dispatch.cast().ok()
    }

    unsafe fn path_from_web(web: &IWebBrowser2) -> Option<PathBuf> {
        if let Some(browser) = top_level_browser(web) {
            if let Some(path) = path_from_browser(&browser) {
                return Some(path);
            }
        }
        path_from_automation(web)
    }

    unsafe fn top_level_browser(web: &IWebBrowser2) -> Option<IShellBrowser> {
        let provider: IServiceProvider = web.cast().ok()?;
        provider.QueryService(&SID_STopLevelBrowser).ok()
    }

    unsafe fn browser_tab_hwnd(browser: &IShellBrowser) -> windows::core::Result<HWND> {
        let ole = browser.cast::<IOleWindow>()?;
        ole.GetWindow()
    }

    unsafe fn path_from_browser(browser: &IShellBrowser) -> Option<PathBuf> {
        let view = browser.QueryActiveShellView().ok()?;
        path_from_view(&view)
    }

    fn same_hwnd(shell_hwnd: isize, foreground: HWND) -> bool {
        if shell_hwnd == 0 || foreground.0.is_null() {
            return false;
        }
        shell_hwnd == foreground.0 as isize || shell_hwnd as i32 == foreground.0 as isize as i32
    }

    unsafe fn path_from_view(view: &IShellView) -> Option<PathBuf> {
        let folder_view: IFolderView = view.cast().ok()?;
        // Ctrl+click leaves the focus rectangle on an item after deselecting it.
        // An empty selection is not a file to preview.
        let count = folder_view.ItemCount(SVGIO_SELECTION).ok()?;
        if count <= 0 {
            return None;
        }
        let (focused, focused_is_selected) = focused_candidate(&folder_view);
        let first_selected = if focused.is_some() && focused_is_selected {
            None
        } else {
            first_selection_path(&folder_view)
        };
        choose_selected(focused, focused_is_selected, first_selected)
    }

    unsafe fn focused_candidate(folder_view: &IFolderView) -> (Option<PathBuf>, bool) {
        let Ok(index) = folder_view.GetFocusedItem() else {
            return (None, false);
        };
        if index < 0 {
            return (None, false);
        }
        let Ok(pidl) = folder_view.Item(index) else {
            return (None, false);
        };
        if pidl.is_null() {
            return (None, false);
        }
        let selected = pidl_is_selected(folder_view, pidl);
        let path = if selected {
            path_from_pidl(folder_view, pidl)
        } else {
            None
        };
        ILFree(Some(pidl));
        (path, selected)
    }

    unsafe fn pidl_is_selected(folder_view: &IFolderView, pidl: *const ITEMIDLIST) -> bool {
        let Ok(view) = folder_view.cast::<IFolderView2>() else {
            return false;
        };
        match view.GetSelectionState(pidl) {
            Ok(state) => (state & (SVSI_SELECT.0 as u32)) != 0,
            Err(_) => false,
        }
    }

    unsafe fn first_selection_path(folder_view: &IFolderView) -> Option<PathBuf> {
        let items: IEnumIDList = folder_view.Items(SVGIO_SELECTION).ok()?;
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        let mut fetched = 0u32;
        let hr = items.Next(std::slice::from_mut(&mut pidl), Some(&mut fetched));
        if hr.is_ok() && fetched > 0 && !pidl.is_null() {
            let path = path_from_pidl(folder_view, pidl);
            ILFree(Some(pidl));
            path
        } else {
            if !pidl.is_null() {
                ILFree(Some(pidl));
            }
            None
        }
    }

    unsafe fn path_from_pidl(folder_view: &IFolderView, pidl: *mut ITEMIDLIST) -> Option<PathBuf> {
        if pidl.is_null() {
            return None;
        }
        let folder: IShellFolder = folder_view.GetFolder().ok()?;
        let item: IShellItem = SHCreateItemWithParent(None, &folder, pidl).ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        if name.is_null() {
            return None;
        }
        let path = path_from_wide(name.as_wide());
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }

    fn path_from_wide(wide: &[u16]) -> Option<PathBuf> {
        if wide.is_empty() {
            None
        } else {
            Some(PathBuf::from(OsString::from_wide(wide)))
        }
    }

    unsafe fn path_from_automation(web: &IWebBrowser2) -> Option<PathBuf> {
        let document = web.Document().ok()?;
        let dual: IShellFolderViewDual = document.cast().ok()?;
        let items = dual.SelectedItems().ok()?;
        let count = items.Count().ok()?;
        if count <= 0 {
            return None;
        }
        let first_selected = folder_item_at(&items, 0);
        let focused = dual
            .FocusedItem()
            .ok()
            .and_then(|item| folder_item_path(&item));
        let focused_is_selected = match &focused {
            Some(path) => selection_contains_path(&items, count, path),
            None => false,
        };
        choose_selected(focused, focused_is_selected, first_selected)
    }

    unsafe fn folder_item_at(items: &FolderItems, index: i32) -> Option<PathBuf> {
        let index = VARIANT::from(index);
        let item = items.Item(&index).ok()?;
        folder_item_path(&item)
    }

    unsafe fn selection_contains_path(items: &FolderItems, count: i32, path: &Path) -> bool {
        for index in 0..count {
            let variant = VARIANT::from(index);
            let Ok(item) = items.Item(&variant) else {
                continue;
            };
            if folder_item_path(&item).as_deref() == Some(path) {
                return true;
            }
        }
        false
    }

    unsafe fn folder_item_path(item: &FolderItem) -> Option<PathBuf> {
        let bstr = item.Path().ok()?;
        path_from_wide(bstr.as_wide())
    }

    fn register_class(
        instance: HINSTANCE,
        name: &str,
        style: WNDCLASS_STYLES,
    ) -> windows::core::Result<()> {
        let wide = utf16(name);
        unsafe {
            let cursor = LoadCursorW(HINSTANCE::default(), IDC_ARROW)?;
            let brush = GetSysColorBrush(COLOR_WINDOW);
            let class = WNDCLASSW {
                style,
                lpfnWndProc: Some(wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: HICON::default(),
                hCursor: cursor,
                hbrBackground: brush,
                lpszMenuName: PCWSTR::null(),
                lpszClassName: PCWSTR(wide.as_ptr()),
            };
            if RegisterClassW(&class) == 0 {
                return Err(windows::core::Error::from_win32());
            }
        }
        Ok(())
    }

    fn create_message_window(instance: HINSTANCE) -> windows::core::Result<HWND> {
        // A message-only window (`HWND_MESSAGE`) does not receive the broadcast
        // `TaskbarCreated` message, and `FindWindowW` (used by `--quit`) does not
        // see it. This top-level window is never shown.
        let class_name = utf16(MESSAGE_CLASS);
        let title = utf16("Spacepeek");
        unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW,
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                0,
                0,
                HWND::default(),
                HMENU::default(),
                instance,
                None,
            )
        }
    }

    /// Monospace faces for text previews, in preference order. GDI substitutes
    /// when a face is missing, so the created font is kept only when its actual
    /// face name matches. `WM_DPICHANGED` recreates this through `swap_fonts`.
    const TEXT_FACES: [&str; 2] = ["Cascadia Mono", "Consolas"];

    fn create_ui_font(dpi: u32) -> HFONT {
        let height = -scaled_font_height(dpi);
        // FIXED_PITCH | FF_MODERN asks for a monospace substitute if the face is missing.
        let pitch = u32::from(FIXED_PITCH.0) | u32::from(FF_MODERN.0);
        let mut fallback = HFONT::default();
        for face in TEXT_FACES {
            let font = create_named_font(height, FW_NORMAL.0 as i32, pitch, face);
            if font.0.is_null() {
                continue;
            }
            if font_face_name(font).is_some_and(|actual| actual.eq_ignore_ascii_case(face)) {
                discard_font(fallback);
                return font;
            }
            if fallback.0.is_null() {
                fallback = font;
            } else {
                discard_font(font);
            }
        }
        fallback
    }

    fn create_named_font(height: i32, weight: i32, pitch: u32, face: &str) -> HFONT {
        let face = utf16(face);
        unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                u32::from(DEFAULT_CHARSET.0),
                0,
                0,
                u32::from(CLEARTYPE_QUALITY.0),
                pitch,
                PCWSTR(face.as_ptr()),
            )
        }
    }

    fn font_face_name(font: HFONT) -> Option<String> {
        unsafe {
            let dc = GetDC(HWND::default());
            if dc.is_invalid() {
                return None;
            }
            let previous = SelectObject(dc, font);
            let mut buf = [0u16; 64];
            let copied = GetTextFaceW(dc, Some(&mut buf));
            if !previous.is_invalid() {
                let _ = SelectObject(dc, previous);
            }
            let _ = ReleaseDC(HWND::default(), dc);
            if copied <= 1 {
                return None;
            }
            let len = usize::try_from(copied).unwrap_or(0).saturating_sub(1);
            let len = len.min(buf.len());
            String::from_utf16(&buf[..len]).ok()
        }
    }

    struct UiFonts {
        edit: HFONT,
        header: HFONT,
        detail: HFONT,
    }

    fn swap_fonts(dpi: u32) -> (UiFonts, StaleFonts) {
        let dpi = effective_dpi(dpi);
        with_app_mut(|app| {
            if app.font_dpi == dpi
                && !app.font.0.is_null()
                && !app.header_font.0.is_null()
                && !app.detail_font.0.is_null()
            {
                return (
                    UiFonts {
                        edit: app.font,
                        header: app.header_font,
                        detail: app.detail_font,
                    },
                    StaleFonts(UiFonts {
                        edit: HFONT::default(),
                        header: HFONT::default(),
                        detail: HFONT::default(),
                    }),
                );
            }
            let edit = create_ui_font(dpi);
            let ui_pitch = u32::from(VARIABLE_PITCH.0) | u32::from(FF_SWISS.0);
            let header = create_named_font(
                -scaled_header_font(dpi),
                FW_SEMIBOLD.0 as i32,
                ui_pitch,
                "Segoe UI",
            );
            let detail = create_named_font(
                -scaled_detail_font(dpi),
                FW_NORMAL.0 as i32,
                ui_pitch,
                "Segoe UI",
            );
            let stale_edit = replace_font(&mut app.font, edit);
            let stale_header = replace_font(&mut app.header_font, header);
            let stale_detail = replace_font(&mut app.detail_font, detail);
            if !app.font.0.is_null() && !app.header_font.0.is_null() && !app.detail_font.0.is_null()
            {
                app.font_dpi = dpi;
            }
            (
                UiFonts {
                    edit: app.font,
                    header: app.header_font,
                    detail: app.detail_font,
                },
                StaleFonts(UiFonts {
                    edit: stale_edit,
                    header: stale_header,
                    detail: stale_detail,
                }),
            )
        })
    }

    fn replace_font(slot: &mut HFONT, next: HFONT) -> HFONT {
        if next.0.is_null() {
            HFONT::default()
        } else {
            std::mem::replace(slot, next)
        }
    }

    fn apply_font(edit: HWND, font: HFONT) {
        if edit.0.is_null() || font.0.is_null() {
            return;
        }
        unsafe {
            let _ = SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
        }
    }

    fn discard_font(font: HFONT) {
        if font.0.is_null() {
            return;
        }
        unsafe {
            let _ = DeleteObject(font);
        }
    }

    fn discard_current_font() {
        let (edit, header, detail) = with_app_mut(|app| {
            app.font_dpi = 0;
            (
                std::mem::take(&mut app.font),
                std::mem::take(&mut app.header_font),
                std::mem::take(&mut app.detail_font),
            )
        });
        discard_font(edit);
        discard_font(header);
        discard_font(detail);
    }

    /// Deletes fonts the window no longer uses. Drop runs after the new fonts
    /// have been selected into the controls.
    struct StaleFonts(UiFonts);

    impl Drop for StaleFonts {
        fn drop(&mut self) {
            discard_font(self.0.edit);
            discard_font(self.0.header);
            discard_font(self.0.detail);
        }
    }

    fn begin_shutdown(hwnd: HWND) {
        if SHUTTING_DOWN.swap(true, Ordering::AcqRel) {
            return;
        }
        with_app_mut(|app| {
            let _ = app.cancel.generate();
            app.state = PreviewState::Hidden;
            publish_session(&app.state, app.explorer_hwnd);
        });
        sync_watch_timer();
        hide_preview(false);
        discard_current_font();
        remove_tray_icon(hwnd);
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }

    fn register_taskbar_created() {
        let msg = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        TASKBAR_CREATED.store(msg, Ordering::Release);
    }

    fn add_tray_icon(hwnd: HWND) {
        unsafe {
            let mut data = tray_icon_data(hwnd);
            if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                    debug_log("Spacepeek: notification icon was not added\n");
                    return;
                }
            }
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let version4 = Shell_NotifyIconW(NIM_SETVERSION, &data).as_bool();
            TRAY_V4.store(version4, Ordering::Release);
            if !version4 {
                debug_log("Spacepeek: notification icon version was not set\n");
            }
        }
    }

    fn remove_tray_icon(hwnd: HWND) {
        if hwnd.0.is_null() {
            return;
        }
        unsafe {
            let data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_ID,
                ..NOTIFYICONDATAW::default()
            };
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
    }

    fn tray_icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_SPACEPEEK_TRAY,
            ..NOTIFYICONDATAW::default()
        };
        data.hIcon = unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() };
        write_wide_z(&mut data.szTip, "Spacepeek");
        data
    }

    fn write_wide_z(dst: &mut [u16], text: &str) {
        let max = dst.len().saturating_sub(1);
        for (slot, unit) in dst.iter_mut().zip(text.encode_utf16().take(max)) {
            *slot = unit;
        }
    }

    fn on_tray_message(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
        let event = (lparam.0 as u32) & 0xffff;
        let version4 = TRAY_V4.load(Ordering::Acquire);
        if !tray_event_opens_menu(event, version4) {
            return;
        }
        let (x, y) = if version4 {
            // `GET_X_LPARAM` / `GET_Y_LPARAM` of wParam under NOTIFYICON_VERSION_4.
            lparam_xy(wparam.0)
        } else {
            cursor_pos()
        };
        show_exit_menu(hwnd, x, y);
    }

    fn tray_event_opens_menu(event: u32, version4: bool) -> bool {
        if version4 {
            // Version 4 also delivers the raw button-up messages. Handling both
            // opens the menu twice. `NIN_SELECT` is a left click; `WM_CONTEXTMENU`
            // is a right click and the keyboard menu key.
            event == WM_CONTEXTMENU || event == NIN_SELECT || event == NIN_KEYSELECT
        } else {
            event == WM_RBUTTONUP || event == WM_LBUTTONUP || event == WM_CONTEXTMENU
        }
    }

    fn lparam_xy(value: usize) -> (i32, i32) {
        let packed = value as u32;
        let x = packed as i16 as i32;
        let y = (packed >> 16) as i16 as i32;
        (x, y)
    }

    fn cursor_pos() -> (i32, i32) {
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt).is_ok() } {
            (pt.x, pt.y)
        } else {
            (0, 0)
        }
    }

    fn show_exit_menu(hwnd: HWND, x: i32, y: i32) {
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            if AppendMenuW(menu, MF_STRING, CMD_EXIT, w!("Exit Spacepeek")).is_err() {
                let _ = DestroyMenu(menu);
                return;
            }
            // The menu will not dismiss on an outside click unless this window
            // is foreground. `WM_NULL` is the documented task switch afterwards.
            let _ = SetForegroundWindow(hwnd);
            let selected = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
                x,
                y,
                0,
                hwnd,
                None,
            );
            let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            if selected.0 == CMD_EXIT as i32 {
                let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
    }

    fn set_text(hwnd: HWND, text: &str) {
        if hwnd.0.is_null() {
            return;
        }
        let wide = utf16(text);
        unsafe {
            let _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));
        }
    }

    fn utf16(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }
}
