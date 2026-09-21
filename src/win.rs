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
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use catguard::detector::{Rule, Thresholds};
use catguard::guard::{Action, Guard, UNLOCK_WORD};
use catguard::history::{Incident, Mods, UndoStep, LOOKBACK};
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
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
/// Hook thread to UI thread. `wparam` is one of the `GUARD_*` codes.
const WM_GUARD: u32 = WM_APP + 2;
const GUARD_LOCK: usize = 0;
const GUARD_DETER: usize = 1;
const GUARD_PROGRESS: usize = 2;
const GUARD_UNLOCK: usize = 3;
/// Keys changed while locked: redraw the timeline.
const GUARD_REFRESH: usize = 4;

const ID_PAUSE: usize = 1;
const ID_AUTOSTART: usize = 2;
const ID_EXIT: usize = 3;
const ID_INCIDENT: usize = 4;

const BTN_HUMAN: usize = 10;
const BTN_UNDO: usize = 11;
const BTN_CLOSE: usize = 12;

const ICON_ACTIVE: usize = 2;
const ICON_PAUSED: usize = 3;

const POLL_MS: u32 = 25;

static PAUSED: AtomicBool = AtomicBool::new(false);
static UNLOCK_CLICKED: AtomicBool = AtomicBool::new(false);
/// The lock window shows the incident after the unlock instead of the lock.
static IN_REVIEW: AtomicBool = AtomicBool::new(false);

/// A key reached the programs after the last unlock. Backspace would then
/// delete what the human typed, not what the cat typed.
static TYPED_SINCE_UNLOCK: AtomicBool = AtomicBool::new(false);

/// The hook thread writes a fresh snapshot here whenever it posts `WM_GUARD`,
/// which only happens around a lock. Normal typing never touches this mutex.
static INCIDENT: Mutex<Option<Incident>> = Mutex::new(None);

/// The window that had the focus when the lock fell. Undo only types into it.
static TARGET: Mutex<(isize, String)> = Mutex::new((0, String::new()));

/// Window handles as integers, because raw pointers are not `Sync`.
struct Ui {
    main: isize,
    lock: isize,
    detail: isize,
    progress: isize,
    human: isize,
    summary: isize,
    undo: isize,
    close: isize,
    small_font: isize,
    dpi: i32,
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
        let mut ui = create_lock_window(instance);
        ui.main = main as isize;
        let _ = UI.set(ui);
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
        let now = state.started.elapsed().as_micros() as u64;
        let verdict = if message == WM_KEYDOWN || message == WM_SYSKEYDOWN {
            let verdict = state.guard.key_down(key, event.vkCode as u8, now);
            if !verdict.swallow {
                TYPED_SINCE_UNLOCK.store(true, Relaxed);
            }
            verdict
        } else {
            state.guard.key_up(key, now)
        };

        if verdict.action.is_some() || state.guard.is_locked() {
            *INCIDENT.lock().unwrap() = state.guard.incident(now);
            post(verdict.action);
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
            *INCIDENT.lock().unwrap() = state.guard.incident(now);
            post(Some(action));
        }
        if state.guard.is_idle() {
            unsafe { KillTimer(null_mut(), state.timer) };
            state.timer = 0;
        }
    });
}

fn post(action: Option<Action>) {
    let Some(ui) = UI.get() else { return };
    let (code, detail) = match action {
        Some(Action::Lock(rule)) => (GUARD_LOCK, rule as isize),
        Some(Action::Deter) => (GUARD_DETER, 0),
        Some(Action::Progress(n)) => (GUARD_PROGRESS, n as isize),
        Some(Action::Unlock) => (GUARD_UNLOCK, 0),
        None => (GUARD_REFRESH, 0),
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
                GUARD_DETER => {
                    play(ui);
                    refresh(ui);
                }
                GUARD_PROGRESS => {
                    set_progress(ui, lparam as usize);
                    refresh(ui);
                }
                GUARD_UNLOCK => show_review(ui),
                _ => refresh(ui),
            }
            0
        }
        WM_COMMAND if hwnd as isize == ui.lock => {
            match wparam & 0xFFFF {
                BTN_HUMAN => {
                    UNLOCK_CLICKED.store(true, Relaxed);
                    show_review(ui);
                }
                BTN_UNDO => undo(ui),
                _ => hide_lock(ui),
            }
            0
        }
        WM_PAINT if hwnd as isize == ui.lock => {
            let mut paint: PAINTSTRUCT = zeroed();
            let hdc = BeginPaint(hwnd, &mut paint);
            paint_timeline(ui, hdc);
            EndPaint(hwnd, &paint);
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

unsafe fn create_lock_window(instance: HINSTANCE) -> Ui {
    let dpi = GetDpiForSystem() as i32;
    let px = |n: i32| n * dpi / 96;
    let (style, ex_style) = (WS_POPUP | WS_DLGFRAME, WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE);
    let mut frame = RECT { left: 0, top: 0, right: px(560), bottom: px(516) };
    AdjustWindowRectEx(&mut frame, style, 0, ex_style);
    let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);

    // WS_EX_NOACTIVATE keeps the focus where the human left it. The hook sees
    // the unlock word anyway, so this window never needs the keyboard. It is
    // also what lets the undo button type into the window behind it.
    let lock = CreateWindowExW(
        ex_style, w!("catguard"), w!("catguard"), style,
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
    let child = |class: *const u16, text: *const u16, style: u32, id: usize, rect: [i32; 4], font: HFONT| {
        let [x, y, w, h] = rect.map(px);
        let hwnd = CreateWindowExW(
            0, class, text, WS_CHILD | WS_VISIBLE | style,
            x, y, w, h, lock, id as HMENU, instance, null(),
        );
        SendMessageW(hwnd, WM_SETFONT, font as WPARAM, 1);
        hwnd as isize
    };

    // SS_CENTER. windows-sys files it under SystemServices, a whole feature
    // for one constant.
    let center = 1;
    let body = font(10, FW_NORMAL as i32, w!("Segoe UI"));
    let button = BS_PUSHBUTTON as u32;
    child(w!("STATIC"), w!("Cat-like typing detected"), center, 0, [24, 18, 512, 34], font(18, FW_SEMIBOLD as i32, w!("Segoe UI")));
    Ui {
        main: 0,
        lock: lock as isize,
        detail: child(w!("STATIC"), null(), center, 0, [24, 56, 512, 40], body),
        progress: child(w!("STATIC"), null(), center, 0, [24, 100, 512, 32], font(16, FW_NORMAL as i32, w!("Consolas"))),
        human: child(w!("BUTTON"), w!("I am human"), button, BTN_HUMAN, [200, 138, 160, 34], body),
        summary: child(w!("STATIC"), null(), 0, 0, [24, 364, 512, 92], body),
        undo: child(w!("BUTTON"), null(), button, BTN_UNDO, [24, 466, 344, 34], body),
        close: child(w!("BUTTON"), w!("Close"), button, BTN_CLOSE, [384, 466, 152, 34], body),
        small_font: font(9, FW_NORMAL as i32, w!("Segoe UI")) as isize,
        dpi,
        wav: Box::leak(harmonica_wav().into_boxed_slice()),
        taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
    }
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

    let focus = GetForegroundWindow();
    let mut title = [0u16; 128];
    let len = GetWindowTextW(focus, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
    *TARGET.lock().unwrap() = (focus as isize, String::from_utf16_lossy(&title[..len]));

    set_mode(ui, false);
    play(ui);
}

/// After the unlock the window stays, without the lock, when something got
/// through that the human should know about.
unsafe fn show_review(ui: &Ui) {
    let anything = INCIDENT.lock().unwrap().as_ref().is_some_and(|i| !i.leaks().is_empty());
    if !anything {
        return hide_lock(ui);
    }
    SetWindowTextW(
        ui.detail as HWND,
        wide("Unlocked. The lime keys reached your programs before the lock fell.").as_ptr(),
    );
    set_mode(ui, true);
}

unsafe fn set_mode(ui: &Ui, review: bool) {
    IN_REVIEW.store(review, Relaxed);
    TYPED_SINCE_UNLOCK.store(false, Relaxed);
    for (hwnd, visible) in [(ui.progress, !review), (ui.human, !review), (ui.undo, review), (ui.close, review)] {
        ShowWindow(hwnd as HWND, if visible { SW_SHOWNA } else { SW_HIDE });
    }
    refresh(ui);
    SetWindowPos(ui.lock as HWND, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
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

// ------------------------------------------------------- incident: text, undo

/// The name Windows has for a key in the current keyboard language.
unsafe fn key_name(scancode: u32, extended: bool) -> String {
    let mut name = [0u16; 32];
    let lparam = (scancode << 16) | (u32::from(extended) << 24);
    let len = GetKeyNameTextW(lparam as i32, name.as_mut_ptr(), name.len() as i32).max(0) as usize;
    String::from_utf16_lossy(&name[..len])
}

unsafe fn combo_name(mods: Mods, vk: u8) -> String {
    let mut name = String::new();
    for (held, label) in [(mods.ctrl, "Ctrl+"), (mods.win, "Win+"), (mods.alt, "Alt+"), (mods.shift, "Shift+")] {
        if held {
            name.push_str(label);
        }
    }
    let key = key_name(MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC), false);
    name + &if key.is_empty() { format!("key {vk:#04X}") } else { key }
}

/// Rewrites the summary and the undo button from the latest snapshot.
unsafe fn refresh(ui: &Ui) {
    let incident = INCIDENT.lock().unwrap().clone();
    let (mut typed, mut lines) = (Vec::new(), Vec::new());
    let mut steps = Vec::new();
    if let Some(incident) = &incident {
        for leak in incident.leaks() {
            let times = if leak.count > 1 { format!(" \u{d7}{}", leak.count) } else { String::new() };
            let name = combo_name(leak.mods, leak.vk);
            match leak.note {
                Some(note) => lines.push(format!("{name}{times} {note}.")),
                None => typed.push(format!("{name}{times}")),
            }
        }
        steps = incident.undo_plan();
    }
    if !typed.is_empty() {
        lines.insert(0, format!("Typed: {}", typed.join(", ")));
    }
    if lines.is_empty() {
        lines.push("Nothing reached your programs.".into());
    }
    lines.truncate(4);

    let mut label = Vec::new();
    for step in &steps {
        label.push(match step {
            UndoStep::Press(mods, vk) => format!("press {}", combo_name(*mods, *vk)),
            UndoStep::Backspace(n) => format!("remove {n} characters"),
        });
    }
    if !steps.is_empty() {
        lines.push(format!("Undo types into: {}", TARGET.lock().unwrap().1));
    }
    let label = if steps.is_empty() { "Nothing to undo with keys".into() } else { format!("Undo: {}", label.join(", ")) };

    SetWindowTextW(ui.summary as HWND, wide(&lines.join("\n")).as_ptr());
    SetWindowTextW(ui.undo as HWND, wide(&label).as_ptr());
    EnableWindow(ui.undo as HWND, i32::from(!steps.is_empty()));
    InvalidateRect(ui.lock as HWND, null(), 1);
}

unsafe fn undo(ui: &Ui) {
    let Some(incident) = INCIDENT.lock().unwrap().clone() else { return };
    let (target, title) = TARGET.lock().unwrap().clone();
    if target == 0 || GetForegroundWindow() as isize != target {
        // Typing Backspace into whatever has the focus now could delete the
        // wrong thing. This window never takes the focus, so clicking into
        // the right window and then on Undo works.
        let text = format!("The focus is no longer in \u{201c}{title}\u{201d}.\nClick into that window, then click Undo again.");
        SetWindowTextW(ui.summary as HWND, wide(&text).as_ptr());
        return;
    }

    let mut steps = incident.undo_plan();
    if TYPED_SINCE_UNLOCK.load(Relaxed) {
        steps.retain(|step| !matches!(step, UndoStep::Backspace(_)));
        if steps.is_empty() {
            let text = "You have typed since the unlock, so Backspace would delete your text, not the cat's.";
            SetWindowTextW(ui.summary as HWND, wide(text).as_ptr());
            return;
        }
    }

    let mut keys: Vec<(u16, bool)> = Vec::new(); // (virtual key, down)
    let tap = |keys: &mut Vec<(u16, bool)>, vk: u16| keys.extend([(vk, true), (vk, false)]);
    for step in steps {
        match step {
            UndoStep::Backspace(n) => (0..n.min(300)).for_each(|_| tap(&mut keys, VK_BACK)),
            UndoStep::Press(mods, vk) => {
                let held: Vec<u16> = [(mods.ctrl, VK_CONTROL), (mods.win, VK_LWIN), (mods.alt, VK_MENU), (mods.shift, VK_SHIFT)]
                    .into_iter()
                    .filter_map(|(on, vk)| on.then_some(vk))
                    .collect();
                keys.extend(held.iter().map(|&m| (m, true)));
                tap(&mut keys, u16::from(vk));
                keys.extend(held.iter().rev().map(|&m| (m, false)));
            }
        }
    }
    // The hook ignores injected input, so these keys pass the guard.
    let inputs: Vec<INPUT> = keys
        .into_iter()
        .map(|(vk, down)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if down { 0 } else { KEYEVENTF_KEYUP }, time: 0, dwExtraInfo: 0 },
            },
        })
        .collect();
    SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32);

    SetWindowTextW(ui.summary as HWND, wide("Done. Check the window behind this one.").as_ptr());
    EnableWindow(ui.undo as HWND, 0);
}

// ------------------------------------------------------------------- timeline

const fn rgb(r: u32, g: u32, b: u32) -> COLORREF {
    r | g << 8 | b << 16
}

const PANEL: COLORREF = rgb(11, 16, 20);
const LIME: COLORREF = rgb(200, 240, 60);
const HUMAN: COLORREF = rgb(170, 178, 184);
const BLOCKED: COLORREF = rgb(84, 92, 98);
const TEXT: COLORREF = rgb(207, 214, 218);

/// One row per key, time from left to right, one bar per press. Lime bars
/// reached the programs while the paw was down, light bars are what was typed
/// before, dark bars were blocked.
unsafe fn paint_timeline(ui: &Ui, hdc: HDC) {
    let px = |n: i32| n * ui.dpi / 96;
    let top = if IN_REVIEW.load(Relaxed) { 104 } else { 184 };
    let panel = RECT { left: px(24), top: px(top), right: px(536), bottom: px(354) };
    let fill = |rect: &RECT, color: COLORREF| {
        let brush = CreateSolidBrush(color);
        FillRect(hdc, rect, brush);
        DeleteObject(brush);
    };
    fill(&panel, PANEL);
    let Some(incident) = INCIDENT.lock().unwrap().clone() else { return };

    let old_font = SelectObject(hdc, ui.small_font as HGDIOBJ);
    SetBkMode(hdc, TRANSPARENT as i32);
    SetTextColor(hdc, TEXT);
    let text = |x: i32, y: i32, s: &str| {
        let s: Vec<u16> = s.encode_utf16().collect();
        TextOutW(hdc, x, y, s.as_ptr(), s.len() as i32);
    };

    let from = incident.locked_at.saturating_sub(LOOKBACK);
    let until = incident.taken_at.max(incident.locked_at + 100_000);
    let (bars_left, bars_right) = (panel.left + px(96), panel.right - px(64));
    let x_of = |t: u64| bars_left + ((t.clamp(from, until) - from) as i64 * i64::from(bars_right - bars_left) / (until - from) as i64) as i32;

    let mut rows: Vec<u16> = Vec::new();
    for press in &incident.presses {
        if !rows.contains(&press.key) {
            rows.push(press.key);
        }
    }
    let row_height = px(15);
    let room = ((panel.bottom - panel.top - px(38)) / row_height).max(1) as usize;
    let rows = &rows[rows.len().saturating_sub(room)..];

    for (i, &key) in rows.iter().enumerate() {
        let y = panel.top + px(20) + i as i32 * row_height;
        text(panel.left + px(8), y - px(2), &key_name(u32::from(key & 0xFF), key & EXTENDED != 0));
        for press in incident.presses.iter().filter(|p| p.key == key) {
            let end = press.up.unwrap_or(incident.taken_at);
            let (x1, x2) = (x_of(press.down), x_of(end).max(x_of(press.down) + px(3)));
            let color = match (press.passed, incident.during_paw(press)) {
                (false, _) => BLOCKED,
                (true, true) => LIME,
                (true, false) => HUMAN,
            };
            fill(&RECT { left: x1, top: y + px(2), right: x2, bottom: y + row_height - px(3) }, color);
            if press.passed || press.up.is_none() {
                let held = (end - press.down) / 1_000;
                text(x2 + px(4), y - px(2), &if press.up.is_none() { format!("{held} ms, still down") } else { format!("{held} ms") });
            }
        }
    }

    let lock_x = x_of(incident.locked_at);
    fill(&RECT { left: lock_x, top: panel.top + px(16), right: lock_x + px(1).max(1), bottom: panel.bottom - px(18) }, TEXT);
    text(lock_x - px(16), panel.top + px(1), "locked");
    text(panel.left + px(8), panel.bottom - px(17), "lime: reached your programs    light: typed before    dark: blocked");
    SelectObject(hdc, old_font);
}

// ----------------------------------------------------------------------- tray

unsafe fn tray(command: NOTIFY_ICON_MESSAGE) {
    let Some(ui) = UI.get() else { return };
    let paused = PAUSED.load(Relaxed);
    let mut data: NOTIFYICONDATAW = zeroed();
    data.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = ui.main as HWND;
    data.uID = 1;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = LoadImageW(
        GetModuleHandleW(null()),
        (if paused { ICON_PAUSED } else { ICON_ACTIVE }) as *const u16,
        IMAGE_ICON,
        GetSystemMetrics(SM_CXSMICON),
        GetSystemMetrics(SM_CYSMICON),
        LR_SHARED,
    );
    let tip = wide(if paused { "catguard (paused)" } else { "catguard is watching the keyboard" });
    data.szTip[..tip.len()].copy_from_slice(&tip);
    Shell_NotifyIconW(command, &data);
}

unsafe fn tray_menu(ui: &Ui) {
    let paused = PAUSED.load(Relaxed);
    let autostart = autostart_enabled();
    let has_incident = INCIDENT.lock().unwrap().is_some();

    let menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING | if has_incident { 0 } else { MF_GRAYED }, ID_INCIDENT, w!("Last incident"));
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
        ID_INCIDENT => {
            SetWindowTextW(ui.detail as HWND, wide("The last time the keyboard was locked.").as_ptr());
            set_mode(ui, true);
        }
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
