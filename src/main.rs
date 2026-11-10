#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("Spacepeek runs on Windows only");
    std::process::ExitCode::from(1)
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
    use std::os::windows::ffi::OsStringExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, Ordering};

    use spacepeek::{
        choose_selected, esc_action, load_text_preview, should_intercept_space, step, CancelToken,
        Effect, EscAction, Event, FocusContext, Generation, KeyPhase, Preview, PreviewError,
        PreviewLimits, PreviewState, RepeatFilter, TrackedKey,
    };
    use windows::core::{w, Interface, PCWSTR, VARIANT};
    use windows::Win32::Foundation::{
        CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, ERROR_SUCCESS, E_FAIL,
        HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        CreateFontW, GetSysColorBrush, CLEARTYPE_QUALITY, COLOR_WINDOW, DEFAULT_CHARSET, FW_NORMAL,
        HFONT,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IServiceProvider,
        CLSCTX_ALL, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Ole::IOleWindow;
    use windows::Win32::System::Threading::{AttachThreadInput, CreateMutexW, GetCurrentThreadId};
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, GetFocus, SetFocus, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE, VK_LWIN, VK_MENU,
        VK_RWIN, VK_SHIFT, VK_SPACE,
    };
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{
        FolderItem, FolderItems, IEnumIDList, IFolderView, IFolderView2, ILFree, IShellBrowser,
        IShellFolder, IShellFolderViewDual, IShellItem, IShellView, IShellWindows, IWebBrowser2,
        SHCreateItemWithParent, SID_STopLevelBrowser, ShellWindows, SIGDN_FILESYSPATH,
        SVGIO_SELECTION, SVSI_SELECT,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow,
        DispatchMessageW, FindWindowExW, GetClassNameW, GetClientRect, GetForegroundWindow,
        GetGUIThreadInfo, GetMessageW, GetSystemMetrics, GetWindowRect, GetWindowThreadProcessId,
        IsWindow, IsWindowVisible, LoadCursorW, MessageBoxW, MoveWindow, PeekMessageW,
        PostMessageW, PostThreadMessageW, RegisterClassW, SendMessageW, SetForegroundWindow,
        SetWindowTextW, SetWindowsHookExW, ShowWindow, TranslateMessage, UnhookWindowsHookEx,
        ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, GUITHREADINFO, HHOOK, HICON, HMENU,
        HWND_MESSAGE, IDC_ARROW, KBDLLHOOKSTRUCT, LLKHF_UP, MB_ICONERROR, MB_OK, MSG, PM_NOREMOVE,
        SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, WA_INACTIVE, WH_KEYBOARD_LL, WINDOW_EX_STYLE,
        WINDOW_STYLE, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_DESTROY, WM_KEYDOWN, WM_QUIT, WM_SETFONT,
        WM_SIZE, WNDCLASSW, WNDCLASS_STYLES, WS_BORDER, WS_CHILD, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
        WS_POPUP, WS_VISIBLE, WS_VSCROLL,
    };

    const PREVIEW_CLASS: &str = "SpacepeekPreview";
    const MESSAGE_CLASS: &str = "SpacepeekMessage";
    const WM_SPACEPEEK_SPACE: u32 = WM_APP + 1;
    const WM_SPACEPEEK_ESC: u32 = WM_APP + 2;
    const WM_SPACEPEEK_LOADED: u32 = WM_APP + 3;
    const EM_SETLIMITTEXT: u32 = 0x00C5;
    const PREVIEW_WIDTH: i32 = 760;
    const PREVIEW_HEIGHT: i32 = 520;

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
        suppress_focus: bool,
        /// Set for the duration of `on_space`. Explorer COM calls pump this STA
        /// thread, so a posted Space can otherwise re-enter `on_space`.
        space_busy: bool,
    }

    static APP: AtomicPtr<RefCell<App>> = AtomicPtr::new(std::ptr::null_mut());
    static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
    /// Preview HWND for the hook thread. Zero when there is no preview window.
    static PREVIEW_HWND: AtomicIsize = AtomicIsize::new(0);
    static PREVIEW_VISIBLE: AtomicBool = AtomicBool::new(false);
    /// UI thread publishes this while `PreviewState` is `Loading`.
    static LOADING: AtomicBool = AtomicBool::new(false);
    /// Explorer HWND that started the in-flight load. Read only while `LOADING` is set.
    static LOADING_EXPLORER: AtomicIsize = AtomicIsize::new(0);
    // Hook-thread only. The low-level hook runs on `spacepeek-hook`.
    thread_local! {
        static REPEATS: RefCell<RepeatFilter> = const { RefCell::new(RepeatFilter::new()) };
    }

    struct HookThread {
        id: u32,
        join: std::thread::JoinHandle<()>,
    }

    pub fn run() -> windows::core::Result<std::process::ExitCode> {
        let held = match acquire_singleton() {
            Ok(Some(handle)) => handle,
            Ok(None) => return Ok(std::process::ExitCode::SUCCESS),
            Err(err) => return Err(err),
        };

        let module = unsafe { GetModuleHandleW(PCWSTR::null())? };
        let instance = HINSTANCE::from(module);
        let font = create_ui_font();
        let app = Box::leak(Box::new(RefCell::new(App {
            state: PreviewState::Hidden,
            cancel: CancelToken::new(),
            msg_hwnd: HWND::default(),
            preview_hwnd: HWND::default(),
            edit_hwnd: HWND::default(),
            explorer_hwnd: HWND::default(),
            instance,
            font,
            suppress_focus: false,
            space_busy: false,
        })));
        APP.store(app, Ordering::Release);

        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            register_class(instance, MESSAGE_CLASS)?;
            register_class(instance, PREVIEW_CLASS)?;
            let msg_hwnd = create_message_window(instance)?;
            app.borrow_mut().msg_hwnd = msg_hwnd;
            MAIN_HWND.store(msg_hwnd.0 as isize, Ordering::Release);

            // The hook cannot share this thread: a slow Explorer COM call on the
            // STA would stall every keystroke, and Windows may silently unhook it.
            let hook = match start_hook_thread(instance) {
                Ok(hook) => hook,
                Err(err) => {
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
            CoUninitialize();
        }

        let _held = held;
        Ok(std::process::ExitCode::SUCCESS)
    }

    pub fn report(text: &str) {
        let body = utf16(text);
        let title = utf16("Spacepeek");
        unsafe {
            MessageBoxW(
                HWND::default(),
                PCWSTR(body.as_ptr()),
                PCWSTR(title.as_ptr()),
                MB_OK | MB_ICONERROR,
            );
        }
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
            if tx.send(Ok(id)).is_err() {
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
            let _ = UnhookWindowsHookEx(hook);
        }
    }

    unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code < 0 {
            return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
        }
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let vk = VIRTUAL_KEY(kb.vkCode as u16);
        let key = if vk == VK_ESCAPE {
            TrackedKey::Esc
        } else if vk == VK_SPACE {
            TrackedKey::Space
        } else {
            return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
        };
        let up = kb.flags.contains(LLKHF_UP);
        // `pass_through` is CancelLoad: the filter's `intercept` flag both acts and
        // swallows, so the latch is cleared after the post. Repeats and key-up then
        // follow that latch and Explorer still sees the whole press.
        let (intercept, pass_through) = if up {
            (false, false)
        } else {
            match key {
                TrackedKey::Esc => esc_hook_flags(),
                TrackedKey::Space => (space_should_intercept(), false),
            }
        };
        let phase = if up { KeyPhase::Up } else { KeyPhase::Down };
        let mut decision = REPEATS.with(|slot| slot.borrow_mut().observe(key, phase, intercept));
        if decision.act {
            let msg = match key {
                TrackedKey::Esc => WM_SPACEPEEK_ESC,
                TrackedKey::Space => WM_SPACEPEEK_SPACE,
            };
            if !post_app(msg) {
                // The down was not taken. Let this event and the rest of the press through.
                REPEATS.with(|slot| slot.borrow_mut().set_swallowed(key, false));
                return CallNextHookEx(HHOOK::default(), code, wparam, lparam);
            }
            if pass_through {
                REPEATS.with(|slot| slot.borrow_mut().set_swallowed(key, false));
                decision.swallow = false;
            }
        }
        if decision.swallow {
            return LRESULT(1);
        }
        CallNextHookEx(HHOOK::default(), code, wparam, lparam)
    }

    /// `(intercept, pass_through)` for an Esc key-down.
    fn esc_hook_flags() -> (bool, bool) {
        match esc_action(
            preview_is_foreground(),
            loading_in_origin_explorer(),
            modifiers_down(),
        ) {
            EscAction::Dismiss => (true, false),
            EscAction::CancelLoad => (true, true),
            EscAction::Ignore => (false, false),
        }
    }

    fn loading_in_origin_explorer() -> bool {
        if !LOADING.load(Ordering::Acquire) {
            return false;
        }
        let bits = LOADING_EXPLORER.load(Ordering::Acquire);
        if bits == 0 {
            return false;
        }
        let explorer = HWND(bits as *mut _);
        unsafe { GetForegroundWindow() == explorer }
    }

    fn publish_loading(state: &PreviewState, explorer: HWND) {
        if matches!(state, PreviewState::Loading { .. }) {
            // HWND first, then the flag, so a hook that sees LOADING also sees this window.
            LOADING_EXPLORER.store(explorer.0 as isize, Ordering::Release);
            LOADING.store(true, Ordering::Release);
        } else {
            LOADING.store(false, Ordering::Release);
            LOADING_EXPLORER.store(0, Ordering::Release);
        }
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

    fn space_should_intercept() -> bool {
        if modifiers_down() {
            return false;
        }
        let foreground = unsafe { GetForegroundWindow() };
        let focus = focus_hwnd(foreground);
        let ctx = FocusContext {
            foreground_class: class_name(foreground),
            focus_class: class_name(focus),
            preview_visible: PREVIEW_VISIBLE.load(Ordering::Acquire),
            modifiers_down: false,
        };
        should_intercept_space(&ctx)
    }

    fn publish_preview_hwnd(hwnd: HWND) {
        PREVIEW_HWND.store(hwnd.0 as isize, Ordering::Release);
    }

    fn post_app(msg: u32) -> bool {
        let hwnd = main_hwnd();
        if hwnd.0.is_null() {
            return false;
        }
        unsafe { PostMessageW(hwnd, msg, WPARAM(0), LPARAM(0)).is_ok() }
    }

    fn modifiers_down() -> bool {
        fn down(vk: VIRTUAL_KEY) -> bool {
            unsafe { GetAsyncKeyState(i32::from(vk.0)) < 0 }
        }
        down(VK_CONTROL) || down(VK_MENU) || down(VK_SHIFT) || down(VK_LWIN) || down(VK_RWIN)
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

    fn class_name(hwnd: HWND) -> String {
        if hwnd.0.is_null() {
            return String::new();
        }
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        if n <= 0 {
            String::new()
        } else {
            String::from_utf16_lossy(&buf[..n as usize])
        }
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
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn preview_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        match msg {
            WM_SIZE => {
                resize_edit(hwnd);
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
            WM_ACTIVATE => {
                let active = (wparam.0 & 0xffff) as u32;
                let suppress = with_app(|app| app.suppress_focus);
                if active == WA_INACTIVE && !suppress {
                    dispatch(Event::FocusLost);
                }
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_DESTROY => {
                with_app_mut(|app| {
                    if app.preview_hwnd == hwnd {
                        app.preview_hwnd = HWND::default();
                        app.edit_hwnd = HWND::default();
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
        let path = unsafe { selected_path(explorer) };
        match path {
            Some(path) => dispatch(Event::SpacePressed(path)),
            None => dispatch(Event::SpacePressed(PathBuf::new())),
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
        let cancelled = with_app(|app| app.cancel.is_cancelled(&msg.gen));
        if cancelled {
            return;
        }
        if load_left_explorer(&msg.gen) {
            return;
        }
        dispatch(Event::LoadFinished(msg.gen, msg.result));
    }

    /// The user left the Explorer window that started this load. Drop the
    /// result instead of showing the preview over whatever is now in front.
    fn load_left_explorer(gen: &Generation) -> bool {
        let explorer = with_app(|app| {
            if matches!(app.state, PreviewState::Loading { gen: cur, .. } if cur == *gen) {
                Some(app.explorer_hwnd)
            } else {
                None
            }
        });
        let Some(explorer) = explorer else {
            return false;
        };
        if unsafe { GetForegroundWindow() } == explorer {
            return false;
        }
        with_app_mut(|app| {
            if matches!(app.state, PreviewState::Loading { gen: cur, .. } if cur == *gen) {
                app.state = PreviewState::Hidden;
                PREVIEW_VISIBLE.store(false, Ordering::Release);
                publish_loading(&app.state, app.explorer_hwnd);
                let _ = app.cancel.generate();
            }
        });
        true
    }

    fn dispatch(event: Event) {
        let effects = with_app_mut(|app| {
            let state = std::mem::replace(&mut app.state, PreviewState::Hidden);
            let (next, effects) = step(state, event, &app.cancel);
            app.state = next;
            PREVIEW_VISIBLE.store(
                !matches!(app.state, PreviewState::Hidden),
                Ordering::Release,
            );
            publish_loading(&app.state, app.explorer_hwnd);
            effects
        });
        for effect in effects {
            apply(effect);
        }
    }

    fn apply(effect: Effect) {
        match effect {
            Effect::StartLoad { gen, path } => start_load(gen, path),
            Effect::ShowWindow => show_preview(),
            Effect::HideWindow { restore_focus } => hide_preview(restore_focus),
            Effect::CancelLoad => {
                with_app(|app| {
                    let _ = app.cancel.generate();
                });
            }
        }
    }

    fn start_load(gen: Generation, path: PathBuf) {
        let (cancel, hwnd) = with_app(|app| (app.cancel.clone(), app.msg_hwnd));
        if path.as_os_str().is_empty() {
            post_load(
                hwnd,
                gen,
                Err(PreviewError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "No file is selected.",
                ))),
            );
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

    fn show_preview() {
        let (title, body, anchor, instance, font, existing) = with_app(|app| {
            let (title, body) = window_content(&app.state);
            (
                title,
                body,
                app.explorer_hwnd,
                app.instance,
                app.font,
                app.preview_hwnd,
            )
        });
        if existing.0.is_null() {
            let (x, y, width, height) = preview_bounds(anchor);
            let class_name = utf16(PREVIEW_CLASS);
            let title_w = utf16(&title);
            let hwnd = unsafe {
                CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                    PCWSTR(class_name.as_ptr()),
                    PCWSTR(title_w.as_ptr()),
                    WS_POPUP | WS_BORDER,
                    x,
                    y,
                    width,
                    height,
                    HWND::default(),
                    HMENU::default(),
                    instance,
                    None,
                )
            };
            let Ok(hwnd) = hwnd else {
                return;
            };
            let edit_class = utf16("EDIT");
            let edit_style = WS_CHILD
                | WS_VISIBLE
                | WS_VSCROLL
                | WINDOW_STYLE(ES_MULTILINE as u32)
                | WINDOW_STYLE(ES_READONLY as u32)
                | WINDOW_STYLE(ES_AUTOVSCROLL as u32);
            let edit = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    PCWSTR(edit_class.as_ptr()),
                    PCWSTR::null(),
                    edit_style,
                    0,
                    0,
                    width,
                    height,
                    hwnd,
                    HMENU::default(),
                    instance,
                    None,
                )
            };
            let Ok(edit) = edit else {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return;
            };
            unsafe {
                let _ = SendMessageW(edit, EM_SETLIMITTEXT, WPARAM(1_048_576), LPARAM(0));
                if !font.0.is_null() {
                    let _ = SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
                }
            }
            set_text(edit, &body);
            resize_edit_pair(hwnd, edit);
            with_app_mut(|app| {
                app.preview_hwnd = hwnd;
                app.edit_hwnd = edit;
                app.suppress_focus = true;
                publish_preview_hwnd(hwnd);
            });
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }
            force_foreground(hwnd);
            unsafe {
                let _ = SetFocus(edit);
            }
            with_app_mut(|app| app.suppress_focus = false);
        } else {
            set_text(existing, &title);
            let edit = with_app(|app| app.edit_hwnd);
            set_text(edit, &body);
            publish_preview_hwnd(existing);
            with_app_mut(|app| app.suppress_focus = true);
            unsafe {
                let _ = ShowWindow(existing, SW_SHOW);
            }
            force_foreground(existing);
            unsafe {
                let _ = SetFocus(edit);
            }
            with_app_mut(|app| app.suppress_focus = false);
        }
    }

    fn hide_preview(restore_focus: bool) {
        let (preview, explorer) = with_app(|app| (app.preview_hwnd, app.explorer_hwnd));
        if preview.0.is_null() {
            return;
        }
        with_app_mut(|app| app.suppress_focus = true);
        unsafe {
            let _ = ShowWindow(preview, SW_HIDE);
            let _ = DestroyWindow(preview);
        }
        with_app_mut(|app| {
            app.preview_hwnd = HWND::default();
            app.edit_hwnd = HWND::default();
            app.suppress_focus = false;
            publish_preview_hwnd(HWND::default());
        });
        if restore_focus && unsafe { IsWindow(explorer).as_bool() } {
            force_foreground(explorer);
        }
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
                let _ = MoveWindow(edit, 0, 0, rc.right - rc.left, rc.bottom - rc.top, true);
            }
        }
    }

    fn preview_bounds(anchor: HWND) -> (i32, i32, i32, i32) {
        let mut rect = RECT::default();
        let anchored = !anchor.0.is_null() && unsafe { GetWindowRect(anchor, &mut rect).is_ok() };
        if anchored && rect.right > rect.left && rect.bottom > rect.top {
            let cx = rect.left + (rect.right - rect.left) / 2;
            let cy = rect.top + (rect.bottom - rect.top) / 2;
            let x = (cx - PREVIEW_WIDTH / 2).max(rect.left);
            let y = (cy - PREVIEW_HEIGHT / 2).max(rect.top);
            return (x, y, PREVIEW_WIDTH, PREVIEW_HEIGHT);
        }
        let sw = unsafe { GetSystemMetrics(SM_CXSCREEN) };
        let sh = unsafe { GetSystemMetrics(SM_CYSCREEN) };
        (
            (sw - PREVIEW_WIDTH) / 2,
            (sh - PREVIEW_HEIGHT) / 2,
            PREVIEW_WIDTH,
            PREVIEW_HEIGHT,
        )
    }

    fn window_content(state: &PreviewState) -> (String, String) {
        match state {
            PreviewState::Showing { path, preview, .. } => {
                let mut body = to_crlf(&preview.content);
                if preview.truncated {
                    if !body.is_empty() {
                        body.push_str("\r\n\r\n");
                    }
                    body.push_str("[preview truncated]");
                }
                (title_for(path), body)
            }
            PreviewState::Failed { path, error, .. } => {
                let title = if path.as_os_str().is_empty() {
                    "Spacepeek".to_string()
                } else {
                    title_for(path)
                };
                (title, to_crlf(&error.to_string()))
            }
            _ => ("Spacepeek".to_string(), String::new()),
        }
    }

    fn title_for(path: &std::path::Path) -> String {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "Spacepeek".to_string())
    }

    fn to_crlf(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for (i, line) in text.split('\n').enumerate() {
            if i > 0 {
                out.push_str("\r\n");
            }
            out.push_str(line.trim_end_matches('\r'));
        }
        out
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

    fn register_class(instance: HINSTANCE, name: &str) -> windows::core::Result<()> {
        let wide = utf16(name);
        unsafe {
            let cursor = LoadCursorW(HINSTANCE::default(), IDC_ARROW)?;
            let brush = GetSysColorBrush(COLOR_WINDOW);
            let class = WNDCLASSW {
                style: WNDCLASS_STYLES(0),
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
        let class_name = utf16(MESSAGE_CLASS);
        let title = utf16("Spacepeek");
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(title.as_ptr()),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                HMENU::default(),
                instance,
                None,
            )
        }
    }

    fn create_ui_font() -> HFONT {
        let face = utf16("Segoe UI");
        unsafe {
            CreateFontW(
                -18,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                u32::from(DEFAULT_CHARSET.0),
                0,
                0,
                u32::from(CLEARTYPE_QUALITY.0),
                0,
                PCWSTR(face.as_ptr()),
            )
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
