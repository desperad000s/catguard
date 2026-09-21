//! Windows shell around the core.
//!
//! Two threads, and the split matters. Windows calls a low-level keyboard
//! hook for every keystroke of the whole system and drops the hook silently
//! when it answers too slowly. So the hook thread does nothing but run the
//! guard. It tells the UI thread what happened by posting messages and never
//! waits for it. The other direction uses atomics.
//!
//! The UI thread runs tao's event loop. That loop also pumps the messages of
//! two plain Win32 windows: a hidden one that owns the tray icon, and the
//! lock window, which has to appear at once and must never take the focus.
//! The app window is a WebView2 page. It is created when opened and dropped
//! when closed, so the background process stays small.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering::Relaxed};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use catguard::detector::{Rule, Sensitivity, Thresholds};
use catguard::guard::{Action, Guard};
use catguard::history::{Incident, Mods, UndoStep, LOOKBACK};
use catguard::layout::{is_printable, EXTENDED};
use catguard::settings::Settings;
use catguard::snapshot::{diff, Change, Snapshot};
use catguard::sound::Sound;

use crate::win_state;

use serde_json::{json, Value};
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, DeviceEventFilter, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::windows::{IconExtWindows, WindowExtWindows};
use tao::window::{Icon, Theme, Window, WindowBuilder};
use wry::http::{header::CONTENT_TYPE, Response};
use wry::{WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows};

use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;
use windows_sys::Win32::System::Threading::{
    CreateMutexW, GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_HIGHEST,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const WM_TRAY: u32 = WM_APP + 1;
/// Hook thread to UI thread. `wparam` is one of the `GUARD_*` codes.
const WM_GUARD: u32 = WM_APP + 2;
/// A second catguard.exe was started: show the window of this one.
const WM_OPEN: u32 = WM_APP + 3;
/// UI thread to hook thread: lock the keyboard now.
const WM_LOCK_NOW: u32 = WM_APP + 4;
const GUARD_LOCK: usize = 0;
const GUARD_DETER: usize = 1;
const GUARD_PROGRESS: usize = 2;
const GUARD_UNLOCK: usize = 3;
/// Keys changed while locked: the incident has new bars.
const GUARD_REFRESH: usize = 4;

const ID_PAUSE: usize = 1;
const ID_OPEN: usize = 2;
const ID_EXIT: usize = 3;
const BTN_HUMAN: usize = 10;
const ICON_APP: usize = 1;
const ICON_ACTIVE: usize = 2;
const ICON_PAUSED: usize = 3;

const POLL_MS: u32 = 25;

const TIMER_SNAPSHOT: usize = 2;
const SNAPSHOT_EVERY_MS: u32 = 30_000;
/// Three snapshots half a minute apart: the oldest is 60 to 90 seconds old.
const SNAPSHOTS_KEPT: usize = 3;

/// The links in the app. They open in the default browser. An empty address
/// hides the link.
const LINKS: [(&str, &str); 3] = [
    ("github", "https://github.com/desperad000s/catguard"),
    ("site", "https://webseed.me"),
    ("linkedin", "https://www.linkedin.com/in/hendrik-hohnrath-02b390b3"),
];

/// What reaches tao's event loop from the hook thread, the window procedure
/// and the web page.
enum UserEvent {
    /// Show the app window, on this page if one is given.
    Open(Option<&'static str>),
    /// Something the page shows has changed.
    Push,
    /// A key moved: scancode, down, swallowed. Only sent while the app is open.
    Key(u32, bool, bool),
    /// A JSON message from the page.
    Ipc(String),
    Quit,
}

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

fn notify(event: UserEvent) {
    if let Some(proxy) = PROXY.get() {
        let _ = proxy.send_event(event);
    }
}

static PAUSED: AtomicBool = AtomicBool::new(false);
static LOCKED: AtomicBool = AtomicBool::new(false);
static UNLOCK_CLICKED: AtomicBool = AtomicBool::new(false);
static APP_OPEN: AtomicBool = AtomicBool::new(false);
static APP_HWND: AtomicIsize = AtomicIsize::new(0);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
/// A key reached the programs after the last unlock. Backspace would then
/// delete what the human typed, not what the cat typed.
static TYPED_SINCE_UNLOCK: AtomicBool = AtomicBool::new(false);

static SETTINGS: OnceLock<Mutex<Settings>> = OnceLock::new();
/// Bumped on every change, so that the hook thread notices without a lock.
static SETTINGS_GEN: AtomicU32 = AtomicU32::new(1);

fn settings() -> MutexGuard<'static, Settings> {
    SETTINGS.get_or_init(|| Mutex::new(Settings::load(&settings_path()))).lock().unwrap()
}

fn change_settings(change: impl FnOnce(&mut Settings)) {
    let mut settings = settings();
    change(&mut settings);
    *settings = settings.clone().sanitized();
    let _ = settings.save(&settings_path());
    SETTINGS_GEN.fetch_add(1, Relaxed);
}

fn settings_path() -> PathBuf {
    PathBuf::from(std::env::var_os("APPDATA").unwrap_or_default()).join("catguard").join("settings.json")
}

/// The hook thread writes a fresh snapshot here whenever it posts `WM_GUARD`,
/// which only happens around a lock. Normal typing never touches this mutex.
static INCIDENT: Mutex<Option<Incident>> = Mutex::new(None);

/// The state of the PC at regular intervals while nothing is wrong. A cat
/// rarely gets caught with its first step, so the comparison reaches back
/// past the three seconds of the key history.
static BASELINES: Mutex<VecDeque<Snapshot>> = Mutex::new(VecDeque::new());
/// The oldest baseline at the moment of the last lock.
static BEFORE: Mutex<Option<Snapshot>> = Mutex::new(None);

/// What is different now from before the last lock.
unsafe fn changes() -> Vec<Change> {
    match &*BEFORE.lock().unwrap() {
        Some(before) => diff(before, &win_state::take()),
        None => Vec::new(),
    }
}

/// The state comparison knows whether a switch really is flipped. Where it
/// covers a key, the key history's guess is left out, or Undo would press
/// Caps Lock twice.
fn covered_by_state(step: &UndoStep) -> bool {
    const VK_F24: u8 = 0x87;
    matches!(step, UndoStep::Press(_, vk) if [VK_CAPITAL as u8, VK_NUMLOCK as u8, VK_SCROLL as u8, VK_F24].contains(vk))
        && BEFORE.lock().unwrap().is_some()
}

/// What the UI thread noted when the lock fell.
struct LockInfo {
    /// The window that had the focus. Undo only types into this one.
    target: isize,
    title: String,
    when: String,
    undone: bool,
    undo_note: String,
}

static LOCK_INFO: Mutex<LockInfo> =
    Mutex::new(LockInfo { target: 0, title: String::new(), when: String::new(), undone: false, undo_note: String::new() });

/// Native window handles as integers, because raw pointers are not `Sync`.
struct Ui {
    main: isize,
    lock: isize,
    progress: isize,
    human: isize,
    detail: isize,
    dark_brush: isize,
    lime_brush: isize,
    icon: isize,
    dpi: i32,
    taskbar_created: u32,
}

static UI: OnceLock<Ui> = OnceLock::new();

/// The sound that is playing. `PlaySoundW` reads it while it plays, so it has
/// to stay alive until the next one replaces it.
static PLAYING: Mutex<Vec<u8>> = Mutex::new(Vec::new());

struct App {
    window: Window,
    webview: WebView,
    /// The page has loaded and can take `cg.state(...)`.
    ready: bool,
    page: Option<&'static str>,
}

pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    // For bug reports and for the test under Wine: what catguard reads of the PC.
    if let Some(i) = args.iter().position(|arg| arg == "--dump-state") {
        let state = unsafe { win_state::take() };
        let _ = std::fs::write(args.get(i + 1).map_or("catguard-state.txt", |s| s.as_str()), format!("{state:#?}\n"));
        return;
    }
    let background = args.iter().any(|arg| arg == "--background");
    unsafe {
        CreateMutexW(null(), 0, w!("Local\\catguard-single-instance"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let running = FindWindowW(w!("catguard-main"), null());
            if !running.is_null() {
                PostMessageW(running, WM_OPEN, 0, 0);
            }
            return;
        }
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    // tao registers the keyboard for raw input. While a process that did so
    // is in the foreground, Windows stops calling that process's low-level
    // keyboard hook. That switched the guard off exactly while the app window
    // had the focus. catguard needs no raw input, so it is removed.
    event_loop.set_device_event_filter(DeviceEventFilter::Always);
    let _ = PROXY.set(event_loop.create_proxy());
    unsafe {
        let _ = UI.set(create_native_windows());
        tray(NIM_ADD);
        BASELINES.lock().unwrap().push_back(win_state::take());
        SetTimer(UI.get().unwrap().main as HWND, TIMER_SNAPSHOT, SNAPSHOT_EVERY_MS, None);
    }
    std::thread::spawn(hook_thread);
    if !background {
        notify(UserEvent::Open(None));
    }

    let local = PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default());
    let mut web_context = WebContext::new(Some(local.join("catguard").join("webview")));
    let mut app: Option<App> = None;
    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(UserEvent::Open(page)) => {
                if app.is_none() {
                    match open_app(target, &mut web_context) {
                        Ok(opened) => app = Some(opened),
                        Err(reason) => unsafe {
                            let text = format!("catguard could not open its window. The keyboard is still guarded.\n\nThe window needs the Microsoft Edge WebView2 Runtime, which is part of Windows 11 and of current Windows 10.\n\n{reason}");
                            MessageBoxW(null_mut(), wide(&text).as_ptr(), w!("catguard"), MB_ICONWARNING);
                        },
                    }
                }
                if let Some(app) = &mut app {
                    app.page = page.or(app.page);
                    if app.ready {
                        reveal(app);
                    }
                }
            }
            Event::UserEvent(UserEvent::Push) => {
                if let Some(app) = app.as_ref().filter(|app| app.ready) {
                    push_state(app, false);
                }
            }
            Event::UserEvent(UserEvent::Key(code, down, blocked)) => {
                if let Some(app) = app.as_ref().filter(|app| app.ready) {
                    let _ = app.webview.evaluate_script(&format!("cg.key({code},{down},{blocked})"));
                }
            }
            Event::UserEvent(UserEvent::Ipc(message)) => {
                if let Some(app) = &mut app {
                    on_ipc(app, &message);
                }
            }
            Event::UserEvent(UserEvent::Quit) => *control_flow = ControlFlow::Exit,
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                // Dropping the webview ends the WebView2 processes.
                APP_OPEN.store(false, Relaxed);
                APP_HWND.store(0, Relaxed);
                app = None;
            }
            _ => {}
        }
    });
}

// ---------------------------------------------------------------- hook thread

struct HookState {
    guard: Guard,
    started: Instant,
    timer: usize,
    settings_seen: u32,
}

thread_local! {
    static HOOK: RefCell<HookState> = RefCell::new(HookState {
        guard: Guard::new(Thresholds::default()),
        started: Instant::now(),
        timer: 0,
        settings_seen: 0,
    });
}

fn hook_thread() {
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        HOOK_THREAD.store(GetCurrentThreadId(), Relaxed);
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
            match msg.message {
                WM_TIMER => on_timer(),
                WM_LOCK_NOW => lock_now(),
                _ => {}
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
        // One atomic load per key. The mutex is only taken after a change.
        let generation = SETTINGS_GEN.load(Relaxed);
        if generation != state.settings_seen {
            state.settings_seen = generation;
            let settings = settings().clone();
            state.guard.set_thresholds(Thresholds::for_sensitivity(settings.sensitivity));
            state.guard.set_unlock_word(&settings.word);
        }

        let extended = if event.flags & LLKHF_EXTENDED != 0 { EXTENDED } else { 0 };
        let key = event.scanCode as u16 | extended;
        let now = state.started.elapsed().as_micros() as u64;
        let verdict = if message == WM_KEYDOWN || message == WM_SYSKEYDOWN {
            let verdict = state.guard.key_down(key, event.vkCode as u8, now);
            // Typing into catguard's own window does not change the text the
            // cat typed into, so it does not rule out Backspace.
            if !verdict.swallow && unsafe { GetForegroundWindow() } as isize != APP_HWND.load(Relaxed) {
                TYPED_SINCE_UNLOCK.store(true, Relaxed);
            }
            verdict
        } else {
            state.guard.key_up(key, now)
        };

        if APP_OPEN.load(Relaxed) {
            let down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
            notify(UserEvent::Key(u32::from(key), down, verdict.swallow));
        }
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

fn lock_now() {
    HOOK.with(|state| {
        let state = &mut *state.borrow_mut();
        let now = state.started.elapsed().as_micros() as u64;
        if let Some(action) = state.guard.lock_now(now) {
            *INCIDENT.lock().unwrap() = state.guard.incident(now);
            post(Some(action));
        }
    });
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

// ----------------------------------------------------------------- app window

fn open_app(target: &EventLoopWindowTarget<UserEvent>, web_context: &mut WebContext) -> Result<App, String> {
    let theme = match settings().theme.as_str() {
        "light" => Some(Theme::Light),
        "system" => None,
        _ => Some(Theme::Dark),
    };
    let window = WindowBuilder::new()
        .with_title("catguard")
        .with_inner_size(LogicalSize::new(1180.0, 800.0))
        .with_min_inner_size(LogicalSize::new(760.0, 560.0))
        .with_window_icon(Icon::from_resource(ICON_APP as u16, None).ok())
        .with_theme(theme)
        // Shown once the page has rendered, so that no white frame flashes.
        .with_visible(false)
        .build(target)
        .map_err(|e| e.to_string())?;
    let webview = WebViewBuilder::new_with_web_context(web_context)
        .with_custom_protocol("cg".into(), |_id, request| serve(request.uri().path()))
        .with_url("cg://localhost/index.html")
        .with_ipc_handler(|request| notify(UserEvent::Ipc(request.body().clone())))
        .with_background_color((5, 7, 10, 255))
        .with_browser_accelerator_keys(false)
        .build(&window)
        .map_err(|e| e.to_string())?;
    APP_HWND.store(window.hwnd() as isize, Relaxed);
    Ok(App { window, webview, ready: false, page: None })
}

/// The page and its fonts are compiled into the exe.
fn serve(path: &str) -> Response<Cow<'static, [u8]>> {
    let (mime, body): (&str, &'static [u8]) = match path {
        "/index.html" => ("text/html; charset=utf-8", include_bytes!("../ui/index.html")),
        "/fonts/barlow-condensed-600.woff2" => ("font/woff2", include_bytes!("../ui/fonts/barlow-condensed-600.woff2")),
        "/fonts/barlow-condensed-700.woff2" => ("font/woff2", include_bytes!("../ui/fonts/barlow-condensed-700.woff2")),
        _ => ("text/plain", b"not found"),
    };
    let status = if body == b"not found" { 404 } else { 200 };
    Response::builder().status(status).header(CONTENT_TYPE, mime).body(Cow::Borrowed(body)).unwrap()
}

fn reveal(app: &mut App) {
    if let Some(page) = app.page.take() {
        let _ = app.webview.evaluate_script(&format!("cg.show('{page}')"));
    }
    app.window.set_visible(true);
    app.window.set_minimized(false);
    app.window.set_focus();
}

fn push_state(app: &App, with_key_names: bool) {
    let state = unsafe { state_json(with_key_names) };
    let _ = app.webview.evaluate_script(&format!("cg.state({state})"));
}

fn on_ipc(app: &mut App, message: &str) {
    let Ok(message) = serde_json::from_str::<Value>(message) else { return };
    let value = &message["value"];
    match message["cmd"].as_str().unwrap_or_default() {
        "ready" => {
            app.ready = true;
            APP_OPEN.store(true, Relaxed);
            push_state(app, true);
            return reveal(app);
        }
        "pause" => unsafe { set_paused(value.as_bool().unwrap_or(false)) },
        "test_sound" => unsafe { play(Some(serde_json::from_value(value.clone()).unwrap_or_default())) },
        "open" => unsafe {
            // Only the addresses compiled in, never one the page hands over.
            if let Some((_, url)) = LINKS.iter().find(|(name, url)| Some(*name) == value.as_str() && !url.is_empty()) {
                ShellExecuteW(null_mut(), w!("open"), wide(url).as_ptr(), null(), null(), SW_SHOWNORMAL);
            }
        },
        "lock_now" => unsafe {
            PostThreadMessageW(HOOK_THREAD.load(Relaxed), WM_LOCK_NOW, 0, 0);
        },
        "undo" => unsafe { undo() },
        "set" => match (message["key"].as_str().unwrap_or_default(), value) {
            ("sensitivity", Value::String(_)) => {
                if let Ok(sensitivity) = serde_json::from_value::<Sensitivity>(value.clone()) {
                    change_settings(|s| s.sensitivity = sensitivity);
                }
            }
            ("word", Value::String(word)) => change_settings(|s| s.word = word.clone()),
            ("theme", Value::String(theme)) => change_settings(|s| s.theme = theme.clone()),
            ("sound", Value::Bool(on)) => change_settings(|s| s.sound = *on),
            ("sound_kind", Value::String(_)) => {
                if let Ok(kind) = serde_json::from_value::<Sound>(value.clone()) {
                    change_settings(|s| s.sound_kind = kind);
                }
            }
            ("autostart", Value::Bool(on)) => unsafe { set_autostart(*on) },
            _ => {}
        },
        _ => {}
    }
    push_state(app, false);
}

/// Everything the page shows, as one JSON object.
unsafe fn state_json(with_key_names: bool) -> Value {
    let settings = settings().clone();
    let changed = changes();
    let info = LOCK_INFO.lock().unwrap();
    let incident = INCIDENT.lock().unwrap().clone().map(|incident| {
        let from = incident.locked_at.saturating_sub(LOOKBACK);
        let until = incident.taken_at.max(incident.locked_at + 100_000);
        let presses: Vec<Value> = incident
            .presses
            .iter()
            .filter(|p| p.up.unwrap_or(until) >= from)
            .map(|p| {
                let kind = if !p.passed { "blocked" } else if incident.during_paw(p) { "cat" } else { "human" };
                json!({ "name": key_label(p.key), "down": p.down, "up": p.up, "kind": kind, "repeats": p.repeats })
            })
            .collect();
        let leaks: Vec<Value> = incident
            .leaks()
            .iter()
            .map(|l| json!({ "name": combo_name(l.mods, l.vk), "count": l.count, "note": l.note }))
            .collect();
        let undo: Vec<String> = changed
            .iter()
            .filter_map(Change::undo)
            .chain(incident.undo_plan().iter().filter(|step| !covered_by_state(step)).map(|step| match step {
                UndoStep::Press(mods, vk) => format!("press {}", combo_name(*mods, *vk)),
                UndoStep::Backspace(n) => format!("remove {n} characters"),
            }))
            .collect();
        let changes: Vec<Value> = changed
            .iter()
            .map(|c| json!({ "text": c.text(), "advice": c.advice(), "undo": c.undo().is_some() }))
            .collect();
        json!({
            "when": info.when, "rule": rule_name(incident.rule), "target": info.title,
            "from": from, "until": until, "locked_at": incident.locked_at,
            "presses": presses, "leaks": leaks, "changes": changes, "has_baseline": BEFORE.lock().unwrap().is_some(), "undo": undo, "undone": info.undone, "undo_note": info.undo_note,
        })
    });
    let key_names = with_key_names.then(|| {
        let names: serde_json::Map<String, Value> = (0u16..0x60)
            .filter(|&code| is_printable(code) && code != 0x39)
            .map(|code| (code.to_string(), Value::from(key_label(code))))
            .collect();
        Value::Object(names)
    });
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "paused": PAUSED.load(Relaxed),
        "locked": LOCKED.load(Relaxed),
        "key_names": key_names,
        "keyboard": { "iso": keyboard_is_iso(), "laptop": has_battery() },
        "links": LINKS.iter().filter(|(_, url)| !url.is_empty()).map(|(name, _)| *name).collect::<Vec<_>>(),
        "stats": { "locks": settings.locks },
        "settings": {
            "sensitivity": settings.sensitivity, "sound": settings.sound, "sound_kind": settings.sound_kind, "word": settings.word,
            "theme": settings.theme, "autostart": autostart_enabled(),
        },
        "incident": incident,
    })
}

fn rule_name(rule: Rule) -> &'static str {
    match rule {
        Rule::Slam => "slam",
        Rule::Chord => "chord",
        Rule::Pair => "pair",
        Rule::Sit => "sit",
        Rule::Manual => "manual",
    }
}

/// What is printed on a key: the character for keys that type one, the name
/// Windows has for the others. Dead keys count as characters here. Their
/// names ("ZIRKUMFLEX", "AKUT") are not what the keycap shows.
unsafe fn key_label(key: u16) -> String {
    if is_printable(key) {
        let vk = MapVirtualKeyW(u32::from(key), MAPVK_VSC_TO_VK);
        // The top bit marks a dead key, the rest is the character.
        let character = MapVirtualKeyW(vk, MAPVK_VK_TO_CHAR) & 0x7FFF_FFFF;
        if let Some(c) = char::from_u32(character).filter(|c| !c.is_control() && *c != ' ') {
            return c.to_string();
        }
    }
    key_name(u32::from(key & 0xFF), key & EXTENDED != 0)
}

/// Windows does not know the shape of the keyboard. The layout language is
/// the best hint: US English keyboards are ANSI, nearly all others are ISO,
/// with the tall Enter and the extra key beside the left Shift.
unsafe fn keyboard_is_iso() -> bool {
    GetKeyboardLayout(0) as usize & 0xFFFF != 0x0409
}

/// A machine with a battery is a laptop, and laptops have an Fn key.
unsafe fn has_battery() -> bool {
    let mut status: SYSTEM_POWER_STATUS = zeroed();
    GetSystemPowerStatus(&mut status) != 0 && status.BatteryFlag & 128 == 0 && status.BatteryFlag != 255
}

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

/// Takes back what keys can take back, in the window the cat typed into.
unsafe fn undo() {
    let Some(incident) = INCIDENT.lock().unwrap().clone() else { return };
    let (target, title) = {
        let info = LOCK_INFO.lock().unwrap();
        (info.target, info.title.clone())
    };
    let note = |text: String| LOCK_INFO.lock().unwrap().undo_note = text;

    let reversible: Vec<Change> = changes().into_iter().filter(|c| c.undo().is_some()).collect();
    let mut steps = incident.undo_plan();
    steps.retain(|step| !covered_by_state(step));
    if TYPED_SINCE_UNLOCK.load(Relaxed) {
        let before = steps.len();
        steps.retain(|step| !matches!(step, UndoStep::Backspace(_)));
        if steps.len() < before {
            note("You have typed since the unlock, so Backspace would delete your text, not the cat's. It was left out.".into());
        }
    }
    if steps.is_empty() && reversible.is_empty() {
        return;
    }

    // Keys go to the window the cat typed into, never to whatever happens to
    // have the focus. catguard is in the foreground right now, so Windows
    // lets it hand the focus over.
    SetForegroundWindow(target as HWND);
    std::thread::sleep(Duration::from_millis(200));
    let focused = target != 0 && GetForegroundWindow() as isize == target;

    // Switches and settings do not depend on which window has the focus.
    if let Some(before) = &*BEFORE.lock().unwrap() {
        win_state::restore(before, &reversible, if focused { target as HWND } else { GetForegroundWindow() });
    }
    if !steps.is_empty() && !focused {
        note(format!("\"{title}\" is gone or does not take the focus, so no keys were typed into it."));
        steps.clear();
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
    win_state::send_keys(&keys);
    LOCK_INFO.lock().unwrap().undone = true;
}

// ------------------------------------------------------------- native windows

const fn rgb(r: u32, g: u32, b: u32) -> COLORREF {
    r | g << 8 | b << 16
}

const PLATE: COLORREF = rgb(5, 7, 10);
const LIME: COLORREF = rgb(200, 240, 60);
const WHITE: COLORREF = rgb(242, 245, 243);
const MUTED: COLORREF = rgb(147, 160, 167);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(ui) = UI.get() else {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    };
    match message {
        WM_GUARD => {
            match wparam {
                GUARD_LOCK => on_lock(ui, lparam),
                GUARD_DETER => play(None),
                GUARD_PROGRESS => set_progress(ui, lparam as usize),
                GUARD_UNLOCK => on_unlock(ui),
                _ => {}
            }
            if APP_OPEN.load(Relaxed) {
                notify(UserEvent::Push);
            }
            0
        }
        WM_TIMER if wparam == TIMER_SNAPSHOT => {
            // Not while locked: that state is the cat's, not a baseline.
            if !LOCKED.load(Relaxed) && !PAUSED.load(Relaxed) {
                let mut baselines = BASELINES.lock().unwrap();
                if baselines.len() == SNAPSHOTS_KEPT {
                    baselines.pop_front();
                }
                baselines.push_back(win_state::take());
            }
            0
        }
        // 7 = DBT_DEVNODES_CHANGED: a device came, went or changed.
        WM_DEVICECHANGE if wparam == 7 => {
            win_state::DEVICES_DIRTY.store(true, Relaxed);
            1
        }
        WM_OPEN => {
            notify(UserEvent::Open(None));
            0
        }
        // The only control that sends WM_COMMAND is the unlock button.
        WM_COMMAND if hwnd as isize == ui.lock => {
            UNLOCK_CLICKED.store(true, Relaxed);
            on_unlock(ui);
            0
        }
        // The lock window is plate black with white type, and its button is
        // a lime label. Standard controls ask for their colours here.
        WM_CTLCOLORSTATIC => {
            let (hdc, control) = (wparam as HDC, lparam);
            let (text, back, brush) = match control {
                c if c == ui.human => (PLATE, LIME, ui.lime_brush),
                c if c == ui.progress => (LIME, PLATE, ui.dark_brush),
                c if c == ui.detail => (MUTED, PLATE, ui.dark_brush),
                _ => (WHITE, PLATE, ui.dark_brush),
            };
            SetTextColor(hdc, text);
            SetBkColor(hdc, back);
            brush
        }
        WM_PAINT if hwnd as isize == ui.lock => {
            let mut paint: PAINTSTRUCT = zeroed();
            let hdc = BeginPaint(hwnd, &mut paint);
            let mut client: RECT = zeroed();
            GetClientRect(hwnd, &mut client);
            FrameRect(hdc, &client, ui.lime_brush as HBRUSH);
            let size = 72 * ui.dpi / 96;
            DrawIconEx(hdc, (client.right - size) / 2, 22 * ui.dpi / 96, ui.icon as HICON, size, size, 0, null_mut(), DI_NORMAL);
            EndPaint(hwnd, &paint);
            0
        }
        WM_CLOSE if hwnd as isize == ui.lock => 0,
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP => notify(UserEvent::Open(None)),
                WM_RBUTTONUP => tray_menu(ui),
                _ => {}
            }
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

unsafe fn create_native_windows() -> Ui {
    let instance = GetModuleHandleW(null());
    let dark_brush = CreateSolidBrush(PLATE);
    for name in [w!("catguard-main"), w!("catguard-lock")] {
        let class = WNDCLASSW {
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: dark_brush,
            lpszClassName: name,
            ..zeroed()
        };
        RegisterClassW(&class);
    }
    // A hidden top-level window, not a message-only one: only top-level
    // windows hear the TaskbarCreated broadcast, and FindWindowW finds it.
    let main = CreateWindowExW(0, w!("catguard-main"), w!("catguard"), WS_OVERLAPPED, 0, 0, 0, 0, null_mut(), null_mut(), instance, null());

    let dpi = GetDpiForSystem() as i32;
    let px = |n: i32| n * dpi / 96;
    let (width, height) = (px(540), px(356));
    // WS_EX_NOACTIVATE keeps the focus where the human left it. The hook sees
    // the unlock word anyway, so this window never needs the keyboard.
    let lock = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        w!("catguard-lock"), w!("catguard"), WS_POPUP,
        (GetSystemMetrics(SM_CXSCREEN) - width) / 2,
        (GetSystemMetrics(SM_CYSCREEN) - height) / 2,
        width, height, null_mut(), null_mut(), instance, null(),
    );
    // Rounded corners on Windows 11 (DWMWA_WINDOW_CORNER_PREFERENCE = 33,
    // DWMWCP_ROUND = 2). Windows 10 ignores it.
    let round: u32 = 2;
    DwmSetWindowAttribute(lock, 33, (&round as *const u32).cast(), 4);

    let font = |points: i32, weight: i32| {
        CreateFontW(
            -(points * dpi / 72), 0, 0, 0, weight, 0, 0, 0,
            DEFAULT_CHARSET as u32, 0, 0, CLEARTYPE_QUALITY as u32, 0, w!("Segoe UI"),
        )
    };
    // SS_CENTER = 1, SS_NOTIFY = 0x100 (clicks arrive as WM_COMMAND),
    // SS_CENTERIMAGE = 0x200 (centres one line of text vertically).
    let child = |text: *const u16, style: u32, id: usize, rect: [i32; 4], font: HFONT| {
        let [x, y, w, h] = rect.map(px);
        let hwnd = CreateWindowExW(
            0, w!("STATIC"), text, WS_CHILD | WS_VISIBLE | 1 | style,
            x, y, w, h, lock, id as HMENU, instance, null(),
        );
        SendMessageW(hwnd, WM_SETFONT, font as WPARAM, 1);
        hwnd as isize
    };
    child(w!("Cat-like typing detected"), 0, 0, [20, 104, 500, 40], font(20, FW_SEMIBOLD as i32));
    Ui {
        main: main as isize,
        lock: lock as isize,
        detail: child(null(), 0, 0, [20, 150, 500, 52], font(11, FW_NORMAL as i32)),
        progress: child(null(), 0, 0, [20, 208, 500, 44], font(22, FW_SEMIBOLD as i32)),
        human: child(w!("I am human"), 0x100 | 0x200, BTN_HUMAN, [170, 272, 200, 48], font(12, FW_SEMIBOLD as i32)),
        dark_brush: dark_brush as isize,
        lime_brush: CreateSolidBrush(LIME) as isize,
        icon: LoadImageW(instance, ICON_APP as *const u16, IMAGE_ICON, px(72), px(72), 0) as isize,
        dpi,
        taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
    }
}

unsafe fn on_lock(ui: &Ui, rule: isize) {
    const RULES: [(Rule, &str); 5] = [
        (Rule::Manual, "you locked it"),
        (Rule::Slam, "three keys at once"),
        (Rule::Chord, "four keys in one spot"),
        (Rule::Pair, "two neighbouring keys held"),
        (Rule::Sit, "keys held for seconds"),
    ];
    let reason = RULES.iter().find(|(r, _)| *r as isize == rule).map_or("", |(_, text)| text);
    let word = settings().word.clone();
    let text = format!("The keyboard is locked: {reason}.\nType  {word}  to unlock it, or click the button.");
    SetWindowTextW(ui.detail as HWND, wide(&text).as_ptr());
    set_progress(ui, 0);

    let focus = GetForegroundWindow();
    let mut title = [0u16; 128];
    let len = GetWindowTextW(focus, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
    let mut now: SYSTEMTIME = zeroed();
    GetLocalTime(&mut now);
    *LOCK_INFO.lock().unwrap() = LockInfo {
        target: focus as isize,
        title: String::from_utf16_lossy(&title[..len]),
        when: format!("At {:02}:{:02}", now.wHour, now.wMinute),
        undone: false,
        undo_note: String::new(),
    };

    *BEFORE.lock().unwrap() = BASELINES.lock().unwrap().front().cloned();
    LOCKED.store(true, Relaxed);
    TYPED_SINCE_UNLOCK.store(false, Relaxed);
    change_settings(|s| s.locks += 1);
    SetWindowPos(ui.lock as HWND, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    play(None);
}

/// Hides the lock. If keys got through, the app opens on the page that shows
/// them, because that is the moment the human wants to know.
unsafe fn on_unlock(ui: &Ui) {
    LOCKED.store(false, Relaxed);
    TYPED_SINCE_UNLOCK.store(false, Relaxed);
    ShowWindow(ui.lock as HWND, SW_HIDE);
    let leaked = INCIDENT.lock().unwrap().as_ref().is_some_and(|i| !i.leaks().is_empty());
    if leaked || !changes().is_empty() {
        notify(UserEvent::Open(Some("incident")));
    }
}

unsafe fn set_progress(ui: &Ui, typed: usize) {
    let text: String = settings()
        .word
        .chars()
        .enumerate()
        .flat_map(|(i, c)| [if i < typed { c } else { '_' }, ' '])
        .collect();
    SetWindowTextW(ui.progress as HWND, wide(text.trim_end()).as_ptr());
}

/// Plays the chosen deterrent, or `preview` from the settings page even when
/// the sound is switched off.
unsafe fn play(preview: Option<Sound>) {
    let (enabled, chosen) = {
        let settings = settings();
        (settings.sound, settings.sound_kind)
    };
    if preview.is_none() && !enabled {
        return;
    }
    let mut playing = PLAYING.lock().unwrap();
    PlaySoundW(null(), null_mut(), 0); // stop, so that the old buffer is free
    *playing = preview.unwrap_or(chosen).wav();
    PlaySoundW(playing.as_ptr().cast(), null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
}

unsafe fn set_paused(paused: bool) {
    let Some(ui) = UI.get() else { return };
    PAUSED.store(paused, Relaxed);
    LOCKED.store(false, Relaxed);
    ShowWindow(ui.lock as HWND, SW_HIDE);
    tray(NIM_MODIFY);
    notify(UserEvent::Push);
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
    ) as HICON;
    let tip = wide(if paused { "catguard is asleep" } else { "catguard is watching the keyboard" });
    data.szTip[..tip.len()].copy_from_slice(&tip);
    Shell_NotifyIconW(command, &data);
}

unsafe fn tray_menu(ui: &Ui) {
    let paused = PAUSED.load(Relaxed);
    let menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, ID_OPEN, w!("Open catguard"));
    AppendMenuW(menu, MF_STRING, ID_PAUSE, if paused { w!("Wake catguard") } else { w!("Pause") });
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Exit"));

    let mut cursor: POINT = zeroed();
    GetCursorPos(&mut cursor);
    // Without this the menu stays open when the user clicks elsewhere.
    SetForegroundWindow(ui.main as HWND);
    let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, cursor.x, cursor.y, 0, ui.main as HWND, null());
    DestroyMenu(menu);

    match choice as usize {
        ID_OPEN => notify(UserEvent::Open(None)),
        ID_PAUSE => set_paused(!paused),
        ID_EXIT => {
            tray(NIM_DELETE);
            notify(UserEvent::Quit);
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
    // --background: start in the tray without opening the window.
    let command: Vec<u16> = Some(u16::from(b'"'))
        .into_iter()
        .chain(exe.as_os_str().encode_wide())
        .chain("\" --background\0".encode_utf16())
        .collect();
    RegSetKeyValueW(
        HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, REG_SZ,
        command.as_ptr().cast(), (command.len() * 2) as u32,
    );
}
