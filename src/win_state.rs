//! The Windows side of `catguard::snapshot`: reads the state that a cat can
//! change, and puts back the part that a program can put back.

use std::mem::{size_of, zeroed};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Mutex;

use catguard::snapshot::{Change, Device, Snapshot};

use windows_sys::w;
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::*;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Globalization::GetLocaleInfoW;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::Accessibility::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Set when Windows reports that the device tree changed. Listing every
/// device takes tens of milliseconds, so it only happens after such a report.
pub static DEVICES_DIRTY: AtomicBool = AtomicBool::new(true);
static DEVICES: Mutex<Vec<Device>> = Mutex::new(Vec::new());

pub unsafe fn take() -> Snapshot {
    let toggled = |vk: u16| GetKeyState(i32::from(vk)) & 1 != 0;
    Snapshot {
        caps_lock: toggled(VK_CAPITAL),
        num_lock: toggled(VK_NUMLOCK),
        scroll_lock: toggled(VK_SCROLL),
        sticky_keys: sticky_keys().dwFlags & SKF_STICKYKEYSON != 0,
        filter_keys: filter_keys().dwFlags & FKF_FILTERKEYSON != 0,
        toggle_keys: toggle_keys().dwFlags & TKF_TOGGLEKEYSON != 0,
        language: language(),
        rotation: display_mode().map_or(0, |mode| mode.Anonymous1.Anonymous2.dmDisplayOrientation),
        touchpad: read_dword(HKEY_CURRENT_USER, w!("Software\\Microsoft\\Windows\\CurrentVersion\\PrecisionTouchPad\\Status"), w!("Enabled")).map(|v| v != 0),
        flight_mode: read_dword(HKEY_LOCAL_MACHINE, w!("SYSTEM\\CurrentControlSet\\Control\\RadioManagement\\SystemRadioState"), null()).map(|v| v != 0),
        windows: windows(),
        devices: devices(),
    }
}

unsafe fn read_dword(root: HKEY, key: *const u16, value: *const u16) -> Option<u32> {
    let (mut data, mut size) = (0u32, 4u32);
    let result = RegGetValueW(root, key, value, RRF_RT_REG_DWORD, null_mut(), (&mut data as *mut u32).cast(), &mut size);
    (result == ERROR_SUCCESS).then_some(data)
}

// ----------------------------------------------- accessibility key features

unsafe fn sticky_keys() -> STICKYKEYS {
    let mut keys = STICKYKEYS { cbSize: size_of::<STICKYKEYS>() as u32, dwFlags: 0 };
    SystemParametersInfoW(SPI_GETSTICKYKEYS, keys.cbSize, (&mut keys as *mut STICKYKEYS).cast(), 0);
    keys
}

unsafe fn filter_keys() -> FILTERKEYS {
    let mut keys: FILTERKEYS = zeroed();
    keys.cbSize = size_of::<FILTERKEYS>() as u32;
    SystemParametersInfoW(SPI_GETFILTERKEYS, keys.cbSize, (&mut keys as *mut FILTERKEYS).cast(), 0);
    keys
}

unsafe fn toggle_keys() -> TOGGLEKEYS {
    let mut keys = TOGGLEKEYS { cbSize: size_of::<TOGGLEKEYS>() as u32, dwFlags: 0 };
    SystemParametersInfoW(SPI_GETTOGGLEKEYS, keys.cbSize, (&mut keys as *mut TOGGLEKEYS).cast(), 0);
    keys
}

fn with_bit(flags: u32, bit: u32, on: bool) -> u32 {
    if on { flags | bit } else { flags & !bit }
}

// ------------------------------------------------------------ input language

unsafe fn language() -> (usize, String) {
    let thread = GetWindowThreadProcessId(GetForegroundWindow(), null_mut());
    let mut layout = GetKeyboardLayout(thread) as usize;
    if layout == 0 {
        // Console windows do not say. Fall back to this thread's layout.
        layout = GetKeyboardLayout(0) as usize;
    }
    let mut name = [0u16; 96];
    // 2 = LOCALE_SLOCALIZEDDISPLAYNAME, "Deutsch (Deutschland)".
    let len = GetLocaleInfoW((layout & 0xFFFF) as u32, 2, name.as_mut_ptr(), name.len() as i32).max(1) as usize - 1;
    (layout, String::from_utf16_lossy(&name[..len]))
}

// ------------------------------------------------------------------- display

unsafe fn display_mode() -> Option<DEVMODEW> {
    let mut mode: DEVMODEW = zeroed();
    mode.dmSize = size_of::<DEVMODEW>() as u16;
    (EnumDisplaySettingsW(null(), ENUM_CURRENT_SETTINGS, &mut mode) != 0).then_some(mode)
}

unsafe fn rotate_back(from: u32, to: u32) {
    let Some(mut mode) = display_mode() else { return };
    mode.Anonymous1.Anonymous2.dmDisplayOrientation = from;
    if (from ^ to) & 1 == 1 {
        std::mem::swap(&mut mode.dmPelsWidth, &mut mode.dmPelsHeight);
    }
    mode.dmFields = DM_DISPLAYORIENTATION | DM_PELSWIDTH | DM_PELSHEIGHT;
    // Ask first. A mode the driver refuses must never be applied.
    if ChangeDisplaySettingsExW(null(), &mode, null_mut(), CDS_TEST, null()) == DISP_CHANGE_SUCCESSFUL {
        ChangeDisplaySettingsExW(null(), &mode, null_mut(), CDS_UPDATEREGISTRY, null());
    }
}

// ------------------------------------------------------------------- windows

unsafe fn windows() -> Vec<(usize, String)> {
    unsafe extern "system" fn each(hwnd: HWND, list: LPARAM) -> i32 {
        let list = &mut *(list as *mut Vec<(usize, String)>);
        let tool_window = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW != 0;
        if IsWindowVisible(hwnd) != 0 && !tool_window {
            let mut title = [0u16; 128];
            let len = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32).max(0) as usize;
            let title = String::from_utf16_lossy(&title[..len]);
            if !title.is_empty() && title != "Program Manager" && title != "catguard" {
                list.push((hwnd as usize, title));
            }
        }
        1
    }
    let mut list: Vec<(usize, String)> = Vec::new();
    EnumWindows(Some(each), &mut list as *mut _ as LPARAM);
    list
}

// ------------------------------------------------------------------- devices

unsafe fn devices() -> Vec<Device> {
    let mut cached = DEVICES.lock().unwrap();
    if DEVICES_DIRTY.swap(false, Relaxed) {
        *cached = list_devices();
    }
    cached.clone()
}

unsafe fn list_devices() -> Vec<Device> {
    let set = SetupDiGetClassDevsW(null(), null(), null_mut(), DIGCF_PRESENT | DIGCF_ALLCLASSES);
    if set as isize == -1 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut info: SP_DEVINFO_DATA = zeroed();
    info.cbSize = size_of::<SP_DEVINFO_DATA>() as u32;
    let mut index = 0;
    while SetupDiEnumDeviceInfo(set, index, &mut info) != 0 {
        index += 1;
        let mut text = [0u16; 256];
        if SetupDiGetDeviceInstanceIdW(set, &info, text.as_mut_ptr(), text.len() as u32, null_mut()) == 0 {
            continue;
        }
        let id = from_wide(&text);
        // Software devices come and go with every app and are nothing a cat breaks.
        if id.starts_with("SWD\\") {
            continue;
        }
        let mut name = String::new();
        for property in [SPDRP_FRIENDLYNAME, SPDRP_DEVICEDESC] {
            text = [0u16; 256];
            let bytes = (text.len() * 2) as u32;
            if SetupDiGetDeviceRegistryPropertyW(set, &info, property, null_mut(), text.as_mut_ptr().cast(), bytes, null_mut()) != 0 {
                name = from_wide(&text);
                break;
            }
        }
        let (mut status, mut problem) = (0u32, 0u32);
        let has_problem = CM_Get_DevNode_Status(&mut status, &mut problem, info.DevInst, 0) == CR_SUCCESS && status & DN_HAS_PROBLEM != 0;
        found.push(Device { name: if name.is_empty() { id.clone() } else { name }, id, problem: if has_problem { problem } else { 0 } });
    }
    SetupDiDestroyDeviceInfoList(set);
    found
}

fn from_wide(text: &[u16]) -> String {
    let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
    String::from_utf16_lossy(&text[..len])
}

// ---------------------------------------------------------------------- undo

/// Presses and releases keys as the user would. The hook ignores injected
/// input, so these pass the guard.
pub unsafe fn send_keys(keys: &[(u16, bool)]) {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(vk, down)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT { wVk: vk, wScan: 0, dwFlags: if down { 0 } else { KEYEVENTF_KEYUP }, time: 0, dwExtraInfo: 0 },
            },
        })
        .collect();
    SendInput(inputs.len() as u32, inputs.as_ptr(), size_of::<INPUT>() as i32);
}

/// Puts back every change that `Change::undo` promises to put back.
/// `target` is the window the cat typed into; the input language belongs to it.
pub unsafe fn restore(before: &Snapshot, changes: &[Change], target: HWND) {
    let tap = |vk: u16| send_keys(&[(vk, true), (vk, false)]);
    for change in changes {
        match change {
            Change::CapsLock { .. } => tap(VK_CAPITAL),
            Change::NumLock { .. } => tap(VK_NUMLOCK),
            Change::ScrollLock { .. } => tap(VK_SCROLL),
            Change::StickyKeys { on } => {
                let mut keys = sticky_keys();
                keys.dwFlags = with_bit(keys.dwFlags, SKF_STICKYKEYSON, !on);
                SystemParametersInfoW(SPI_SETSTICKYKEYS, keys.cbSize, (&mut keys as *mut STICKYKEYS).cast(), SPIF_SENDCHANGE);
            }
            Change::FilterKeys { on } => {
                let mut keys = filter_keys();
                keys.dwFlags = with_bit(keys.dwFlags, FKF_FILTERKEYSON, !on);
                SystemParametersInfoW(SPI_SETFILTERKEYS, keys.cbSize, (&mut keys as *mut FILTERKEYS).cast(), SPIF_SENDCHANGE);
            }
            Change::ToggleKeys { on } => {
                let mut keys = toggle_keys();
                keys.dwFlags = with_bit(keys.dwFlags, TKF_TOGGLEKEYSON, !on);
                SystemParametersInfoW(SPI_SETTOGGLEKEYS, keys.cbSize, (&mut keys as *mut TOGGLEKEYS).cast(), SPIF_SENDCHANGE);
            }
            Change::Language { .. } => {
                PostMessageW(target, WM_INPUTLANGCHANGEREQUEST, 0, before.language.0 as LPARAM);
            }
            Change::Rotation { from, to } => rotate_back(*from, *to),
            Change::Touchpad { on: false } => send_keys(&[
                (VK_CONTROL, true), (VK_LWIN, true), (VK_F24, true),
                (VK_F24, false), (VK_LWIN, false), (VK_CONTROL, false),
            ]),
            // The program gets to ask about unsaved work, as with a click on its X.
            Change::WindowOpened { handle, .. } => {
                PostMessageW(*handle as HWND, WM_CLOSE, 0, 0);
            }
            _ => {}
        }
    }
}
