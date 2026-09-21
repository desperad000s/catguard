//! Windows shell around the core: a low-level keyboard hook on its own
//! thread, a tray icon, the lock window and the sound.
//!
//! Two threads, and the split matters. Windows calls a low-level hook for
//! every keystroke of the whole system and drops the hook silently when it
//! answers too slowly. So the hook thread does nothing but run the guard. It
//! tells the UI thread what happened with `PostMessageW` and never waits for
//! it. The other direction uses two atomics.

use std::cell::RefCell;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::OnceLock;
use std::time::Instant;

use catguard::detector::{Rule, Thresholds};
use catguard::guard::{Action, Guard, UNLOCK_WORD};
use catguard::layout::EXTENDED;
use catguard::sound::harmonica_wav;

use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
/// Hook thread to UI thread. `wparam` is one of the `GUARD_*` codes.
const WM_GUARD: u32 = WM_APP + 2;
const GUARD_LOCK: usize = 0;
const GUARD_DETER: usize = 1;
const GUARD_PROGRESS: usize = 2;
const GUARD_UNLOCK: usize = 3;

const ID_PAUSE: usize = 1;
const ID_AUTOSTART: usize = 2;
const ID_EXIT: usize = 3;

const POLL_MS: u32 = 25;

static PAUSED: AtomicBool = AtomicBool::new(false);
static UNLOCK_CLICKED: AtomicBool = AtomicBool::new(false);

/// Window handles as integers, because raw pointers are not `Sync`.
struct Ui {
    main: isize,
    lock: isize,
    detail: isize,
    progress: isize,
    wav: &'static [u8],
    taskbar_created: u32,
}

static UI: OnceLock<Ui> = OnceLock::new();

pub fn run() {
    unsafe {
        CreateMutexW(null(), 0, w!("Local\\catguard-single-instance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_BTNFACE + 1) as HBRUSH,
            lpszClassName: w!("catguard"),
            ..zeroed()
        };
        RegisterClassW(&class);

        // A hidden top-level window, not a message-only one: only top-level
        // windows hear the TaskbarCreated broadcast.
        let main = CreateWindowExW(
            0, class.lpszClassName, w!("catguard"), WS_OVERLAPPED,
            0, 0, 0, 0, null_mut(), null_mut(), instance, null(),
        );
        let (lock, detail, progress) = create_lock_window(instance);

        let _ = UI.set(Ui {
            main: main as isize,
            lock: lock as isize,
            detail: detail as isize,
            progress: progress as isize,
            wav: Box::leak(harmonica_wav().into_boxed_slice()),
            taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
        });
        tray(NIM_ADD);
        std::thread::spawn(hook_thread);

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

// ---------------------------------------------------------------- hook thread

struct HookState {
    guard: Guard,
    started: Instant,
    timer: usize,
}

thread_local! {
    static HOOK: RefCell<HookState> = RefCell::new(HookState {
        guard: Guard::new(Thresholds::default()),
        started: Instant::now(),
        timer: 0,
    });
}

fn hook_thread() {
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), GetModuleHandleW(null()), 0);
        if hook.is_null() {
            MessageBoxW(
                null_mut(),
                w!("Windows refused the keyboard hook, so catguard cannot watch the keyboard."),
                w!("catguard"),
                MB_ICONERROR,
            );
            std::process::exit(1);
        }
        // The hook is called from inside GetMessageW. The only message this
        // thread handles itself is its poll timer.
        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            if msg.message == WM_TIMER {
                on_timer();
            }
        }
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let event = &*(lparam as *const KBDLLHOOKSTRUCT);
        // Injected input comes from software (macros, on-screen keyboards,
        // remote control), never from a paw.
        if event.flags & LLKHF_INJECTED == 0 && on_key(event, wparam as u32) {
            return 1;
        }
    }
    CallNextHookEx(null_mut(), code, wparam, lparam)
}

/// Returns true when the event must not reach the applications.
fn on_key(event: &KBDLLHOOKSTRUCT, message: u32) -> bool {
    HOOK.with(|state| {
        let state = &mut *state.borrow_mut();
        if PAUSED.load(Relaxed) {
            state.guard.reset();
            return false;
        }
        if UNLOCK_CLICKED.swap(false, Relaxed) {
            state.guard.unlock();
        }

        let extended = if event.flags & LLKHF_EXTENDED != 0 { EXTENDED } else { 0 };
        let key = event.scanCode as u16 | extended;
        let verdict = if message == WM_KEYDOWN || message == WM_SYSKEYDOWN {
            let now = state.started.elapsed().as_micros() as u64;
            state.guard.key_down(key, event.vkCode as u8, now)
        } else {
            state.guard.key_up(key)
        };

        if let Some(action) = verdict.action {
            post(action);
        }
        if state.timer == 0 && !state.guard.is_idle() {
            state.timer = unsafe { SetTimer(null_mut(), 0, POLL_MS, None) };
        }
        verdict.swallow
    })
}

fn on_timer() {
    HOOK.with(|state| {
        let state = &mut *state.borrow_mut();
        if UNLOCK_CLICKED.swap(false, Relaxed) {
            state.guard.unlock();
        }
        let now = state.started.elapsed().as_micros() as u64;
        if let Some(action) = state.guard.poll(now) {
            post(action);
        }
        if state.guard.is_idle() {
            unsafe { KillTimer(null_mut(), state.timer) };
            state.timer = 0;
        }
    });
}

fn post(action: Action) {
    let Some(ui) = UI.get() else { return };
    let (code, detail) = match action {
        Action::Lock(rule) => (GUARD_LOCK, rule as isize),
        Action::Deter => (GUARD_DETER, 0),
        Action::Progress(n) => (GUARD_PROGRESS, n as isize),
        Action::Unlock => (GUARD_UNLOCK, 0),
    };
    unsafe { PostMessageW(ui.main as HWND, WM_GUARD, code, detail) };
}

// ------------------------------------------------------------------ UI thread

unsafe extern "system" fn wnd_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(ui) = UI.get() else {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    };
    match message {
        WM_GUARD => {
            match wparam {
                GUARD_LOCK => show_lock(ui, lparam),
                GUARD_DETER => play(ui),
                GUARD_PROGRESS => set_progress(ui, lparam as usize),
                GUARD_UNLOCK => hide_lock(ui),
                _ => {}
            }
            0
        }
        // The only control that sends WM_COMMAND is the unlock button.
        WM_COMMAND if hwnd as isize == ui.lock => {
            UNLOCK_CLICKED.store(true, Relaxed);
            hide_lock(ui);
            0
        }
        WM_CLOSE if hwnd as isize == ui.lock => 0,
        WM_TRAY => {
            if matches!(lparam as u32, WM_LBUTTONUP | WM_RBUTTONUP) {
                tray_menu(ui);
            }
            0
        }
        WM_DESTROY if hwnd as isize == ui.main => {
            tray(NIM_DELETE);
            PostQuitMessage(0);
            0
        }
        _ if message == ui.taskbar_created => {
            // Explorer restarted and lost every tray icon.
            tray(NIM_ADD);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe fn create_lock_window(instance: HINSTANCE) -> (HWND, HWND, HWND) {
    let dpi = GetDpiForSystem() as i32;
    let px = |n: i32| n * dpi / 96;
    let (width, height) = (px(460), px(250));

    // WS_EX_NOACTIVATE keeps the focus where the human left it. The hook sees
    // the unlock word anyway, so this window never needs the keyboard.
    let lock = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        w!("catguard"), w!("catguard"), WS_POPUP | WS_DLGFRAME,
        (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
        (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
        width, height, null_mut(), null_mut(), instance, null(),
    );

    let font = |points: i32, weight: i32, face: *const u16| {
        CreateFontW(
            -(points * dpi / 72), 0, 0, 0, weight, 0, 0, 0,
            DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, face,
        )
    };
    let child = |class: *const u16, text: *const u16, style: u32, rect: [i32; 4], font: HFONT| {
        let [x, y, w, h] = rect.map(px);
        let hwnd = CreateWindowExW(
            0, class, text, WS_CHILD | WS_VISIBLE | style,
            x, y, w, h, lock, null_mut(), instance, null(),
        );
        SendMessageW(hwnd, WM_SETFONT, font as WPARAM, 1);
        hwnd
    };

    // SS_CENTER. windows-sys files it under SystemServices, a whole feature
    // for one constant.
    let center = 1;
    let body_font = font(10, FW_NORMAL as i32, w!("Segoe UI"));
    child(w!("STATIC"), w!("Cat-like typing detected"), center, [24, 20, 412, 36], font(18, FW_SEMIBOLD as i32, w!("Segoe UI")));
    let detail = child(w!("STATIC"), null(), center, [24, 66, 412, 44], body_font);
    let progress = child(w!("STATIC"), null(), center, [24, 116, 412, 34], font(16, FW_NORMAL as i32, w!("Consolas")));
    child(w!("BUTTON"), w!("I am human"), BS_PUSHBUTTON as u32, [150, 164, 160, 36], body_font);
    (lock, detail, progress)
}

unsafe fn show_lock(ui: &Ui, rule: isize) {
    const RULES: [(Rule, &str); 4] = [
        (Rule::Slam, "three keys at once"),
        (Rule::Chord, "four keys in one spot"),
        (Rule::Pair, "two neighbouring keys held"),
        (Rule::Sit, "keys held for seconds"),
    ];
    let reason = RULES.iter().find(|(r, _)| *r as isize == rule).map_or("", |(_, text)| text);
    let text = format!("The keyboard is locked ({reason}).\nType  human  to unlock it, or click the button.");
    SetWindowTextW(ui.detail as HWND, wide(&text).as_ptr());
    set_progress(ui, 0);
    SetWindowPos(ui.lock as HWND, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    play(ui);
}

unsafe fn hide_lock(ui: &Ui) {
    ShowWindow(ui.lock as HWND, SW_HIDE);
}

unsafe fn set_progress(ui: &Ui, typed: usize) {
    let text: String = UNLOCK_WORD
        .iter()
        .enumerate()
        .flat_map(|(i, &c)| [if i < typed { c.to_ascii_lowercase() as char } else { '_' }, ' '])
        .collect();
    SetWindowTextW(ui.progress as HWND, wide(text.trim_end()).as_ptr());
}

unsafe fn play(ui: &Ui) {
    PlaySoundW(ui.wav.as_ptr().cast(), null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
}

unsafe fn tray(command: NOTIFY_ICON_MESSAGE) {
    let Some(ui) = UI.get() else { return };
    let mut data: NOTIFYICONDATAW = zeroed();
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = ui.main as HWND;
    data.uID = 1;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = LoadIconW(null_mut(), IDI_SHIELD);
    let tip = wide(if PAUSED.load(Relaxed) { "catguard (paused)" } else { "catguard is watching the keyboard" });
    data.szTip[..tip.len()].copy_from_slice(&tip);
    Shell_NotifyIconW(command, &data);
}

unsafe fn tray_menu(ui: &Ui) {
    let paused = PAUSED.load(Relaxed);
    let autostart = autostart_enabled();

    let menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, ID_PAUSE, if paused { w!("Resume") } else { w!("Pause") });
    AppendMenuW(menu, MF_STRING | if autostart { MF_CHECKED } else { 0 }, ID_AUTOSTART, w!("Start with Windows"));
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Exit"));

    let mut cursor: POINT = zeroed();
    GetCursorPos(&mut cursor);
    // Without this the menu stays open when the user clicks elsewhere.
    SetForegroundWindow(ui.main as HWND);
    let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, cursor.x, cursor.y, 0, ui.main as HWND, null());
    DestroyMenu(menu);

    match choice as usize {
        ID_PAUSE => {
            PAUSED.store(!paused, Relaxed);
            hide_lock(ui);
            tray(NIM_MODIFY);
        }
        ID_AUTOSTART => set_autostart(!autostart),
        ID_EXIT => {
            DestroyWindow(ui.main as HWND);
        }
        _ => {}
    }
}

// ------------------------------------------------------------------ autostart

const RUN_KEY: *const u16 = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: *const u16 = w!("catguard");

unsafe fn autostart_enabled() -> bool {
    RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, null_mut(), null_mut(), null_mut())
        == ERROR_SUCCESS
}

unsafe fn set_autostart(enable: bool) {
    if !enable {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE);
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let command: Vec<u16> = Some(u16::from(b'"'))
        .into_iter()
        .chain(exe.as_os_str().encode_wide())
        .chain([u16::from(b'"'), 0])
        .collect();
    RegSetKeyValueW(
        HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ,
        command.as_ptr().cast(), (command.len() * 2) as u32,
    );
}
