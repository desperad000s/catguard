//! What is different on the PC after the cat than it was before.
//!
//! Keys tell only half the story. A touchpad can be dead because of Fn+F10,
//! which Windows never sees, or because a driver went missing, which no key
//! explains. So catguard also compares the state of the system from shortly
//! before the lock with the state now. This module is the comparison; the
//! Windows shell fills in the snapshots.

use serde::Serialize;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Device {
    /// The device instance id, which stays the same across snapshots.
    pub id: String,
    pub name: String,
    /// The Device Manager problem code. 0 means the device works.
    pub problem: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub caps_lock: bool,
    pub num_lock: bool,
    pub scroll_lock: bool,
    pub sticky_keys: bool,
    pub filter_keys: bool,
    pub toggle_keys: bool,
    /// The keyboard layout handle and its display name.
    pub language: (usize, String),
    /// 0, 1, 2, 3 for 0, 90, 180, 270 degrees.
    pub rotation: u32,
    /// `None` where the machine has no such thing or Windows does not say.
    pub touchpad: Option<bool>,
    pub flight_mode: Option<bool>,
    /// Open windows as (handle, title).
    pub windows: Vec<(usize, String)>,
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Change {
    CapsLock { on: bool },
    NumLock { on: bool },
    ScrollLock { on: bool },
    StickyKeys { on: bool },
    FilterKeys { on: bool },
    ToggleKeys { on: bool },
    Language { from: String, to: String },
    Rotation { from: u32, to: u32 },
    Touchpad { on: bool },
    FlightMode { on: bool },
    WindowClosed { title: String },
    DeviceGone { name: String },
    DeviceBroken { name: String, problem: u32 },
    DeviceNew { name: String },
}

/// Everything that differs, most worrying first: hardware, then input, then
/// switches, then windows.
pub fn diff(before: &Snapshot, after: &Snapshot) -> Vec<Change> {
    let mut changes = Vec::new();

    for old in &before.devices {
        match after.devices.iter().find(|d| d.id == old.id) {
            None => changes.push(Change::DeviceGone { name: old.name.clone() }),
            Some(now) if now.problem != 0 && old.problem == 0 => {
                changes.push(Change::DeviceBroken { name: now.name.clone(), problem: now.problem });
            }
            Some(_) => {}
        }
    }
    for now in &after.devices {
        if !before.devices.iter().any(|d| d.id == now.id) {
            changes.push(Change::DeviceNew { name: now.name.clone() });
        }
    }

    if let (Some(was), Some(is)) = (before.touchpad, after.touchpad) {
        if was != is {
            changes.push(Change::Touchpad { on: is });
        }
    }
    if let (Some(was), Some(is)) = (before.flight_mode, after.flight_mode) {
        if was != is {
            changes.push(Change::FlightMode { on: is });
        }
    }
    if before.language.0 != after.language.0 {
        changes.push(Change::Language { from: before.language.1.clone(), to: after.language.1.clone() });
    }
    if before.rotation != after.rotation {
        changes.push(Change::Rotation { from: before.rotation, to: after.rotation });
    }

    type Switch = (bool, bool, fn(bool) -> Change);
    let switches: [Switch; 6] = [
        (before.filter_keys, after.filter_keys, |on| Change::FilterKeys { on }),
        (before.sticky_keys, after.sticky_keys, |on| Change::StickyKeys { on }),
        (before.toggle_keys, after.toggle_keys, |on| Change::ToggleKeys { on }),
        (before.caps_lock, after.caps_lock, |on| Change::CapsLock { on }),
        (before.num_lock, after.num_lock, |on| Change::NumLock { on }),
        (before.scroll_lock, after.scroll_lock, |on| Change::ScrollLock { on }),
    ];
    for (was, is, change) in switches {
        if was != is {
            changes.push(change(is));
        }
    }

    for (handle, title) in &before.windows {
        if !after.windows.iter().any(|(h, _)| h == handle) {
            changes.push(Change::WindowClosed { title: title.clone() });
        }
    }
    changes
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

impl Change {
    /// What happened, as a sentence without its full stop.
    pub fn text(&self) -> String {
        match self {
            Change::CapsLock { on } => format!("Caps Lock is {} now", on_off(*on)),
            Change::NumLock { on } => format!("Num Lock is {} now", on_off(*on)),
            Change::ScrollLock { on } => format!("Scroll Lock is {} now", on_off(*on)),
            Change::StickyKeys { on: true } => "Sticky Keys got switched on, which is what five presses of Shift do".into(),
            Change::FilterKeys { on: true } => {
                "Filter Keys got switched on, which is what eight seconds on the right Shift do. Keys now react late or not at all".into()
            }
            Change::ToggleKeys { on: true } => "Toggle Keys got switched on, which is what five seconds on Num Lock do".into(),
            Change::StickyKeys { on: false } => "Sticky Keys got switched off".into(),
            Change::FilterKeys { on: false } => "Filter Keys got switched off".into(),
            Change::ToggleKeys { on: false } => "Toggle Keys got switched off".into(),
            Change::Language { from, to } => format!("The input language changed from {from} to {to}"),
            Change::Rotation { .. } => "The screen got rotated".into(),
            Change::Touchpad { on } => format!("The touchpad got switched {}", on_off(*on)),
            Change::FlightMode { on } => format!("Flight mode got switched {}", on_off(*on)),
            Change::WindowClosed { title } => format!("A window closed: \u{201c}{title}\u{201d}"),
            Change::DeviceGone { name } => format!("A device is gone: {name}"),
            Change::DeviceBroken { name, problem } => {
                let why = match problem {
                    22 => "it got disabled",
                    28 => "its driver is not installed",
                    _ => "Device Manager reports a problem",
                };
                format!("A device stopped working: {name}. Windows says {why} (code {problem})")
            }
            Change::DeviceNew { name } => format!("A device is new: {name}"),
        }
    }

    /// What the human can do where catguard cannot do it for them.
    pub fn advice(&self) -> Option<&'static str> {
        match self {
            Change::DeviceGone { .. } | Change::DeviceBroken { .. } => Some(
                "Open Device Manager and choose Action, Scan for hardware changes. If that does not bring it back, run the driver update of your PC's maker, Lenovo Vantage on a Lenovo.",
            ),
            Change::FlightMode { on: true } => Some("Switch it off in the network menu of the taskbar."),
            Change::Touchpad { on: false } => Some("If Undo does not bring it back: Settings, Bluetooth and devices, Touchpad."),
            _ => None,
        }
    }

    /// What Undo will do about it, or `None` when only the human can.
    /// Things that got switched on by themselves are harmless, so only the
    /// direction a cat causes trouble with is reversed for the touchpad.
    pub fn undo(&self) -> Option<String> {
        Some(match self {
            Change::CapsLock { on } => format!("switch Caps Lock {}", on_off(!on)),
            Change::NumLock { on } => format!("switch Num Lock {}", on_off(!on)),
            Change::ScrollLock { on } => format!("switch Scroll Lock {}", on_off(!on)),
            Change::StickyKeys { on } => format!("switch Sticky Keys {}", on_off(!on)),
            Change::FilterKeys { on } => format!("switch Filter Keys {}", on_off(!on)),
            Change::ToggleKeys { on } => format!("switch Toggle Keys {}", on_off(!on)),
            Change::Language { from, .. } => format!("switch the input language back to {from}"),
            Change::Rotation { .. } => "rotate the screen back".into(),
            Change::Touchpad { on: false } => "press the touchpad key (Ctrl+Win+F24)".into(),
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str, problem: u32) -> Device {
        Device { id: id.into(), name: name.into(), problem }
    }

    fn desk() -> Snapshot {
        Snapshot {
            num_lock: true,
            language: (0x0407, "Deutsch (Deutschland)".into()),
            touchpad: Some(true),
            flight_mode: Some(false),
            windows: vec![(1, "Angebot.docx - Word".into()), (2, "Posteingang - Outlook".into())],
            devices: vec![device("HID\\TP", "HID-compliant touch pad", 0), device("USB\\CAM", "Integrated Camera", 0)],
            ..Snapshot::default()
        }
    }

    #[test]
    fn nothing_changed_means_nothing_to_report() {
        assert_eq!(diff(&desk(), &desk()), []);
    }

    #[test]
    fn the_case_this_was_built_for_a_driver_went_missing() {
        let mut after = desk();
        after.devices[0].problem = 28;
        let changes = diff(&desk(), &after);
        assert_eq!(changes, [Change::DeviceBroken { name: "HID-compliant touch pad".into(), problem: 28 }]);
        assert!(changes[0].text().contains("its driver is not installed"));
        assert!(changes[0].advice().unwrap().contains("Scan for hardware changes"));
        assert_eq!(changes[0].undo(), None);

        let mut gone = desk();
        gone.devices.remove(0);
        assert_eq!(diff(&desk(), &gone), [Change::DeviceGone { name: "HID-compliant touch pad".into() }]);
    }

    #[test]
    fn a_device_that_was_already_broken_is_not_blamed_on_the_cat() {
        let mut before = desk();
        before.devices[1].problem = 22;
        assert_eq!(diff(&before, &before.clone()), []);
    }

    #[test]
    fn what_a_cat_on_the_modifiers_does() {
        let mut after = desk();
        after.filter_keys = true;
        after.caps_lock = true;
        after.language = (0x0409, "English (United States)".into());
        let changes = diff(&desk(), &after);
        assert_eq!(
            changes,
            [
                Change::Language { from: "Deutsch (Deutschland)".into(), to: "English (United States)".into() },
                Change::FilterKeys { on: true },
                Change::CapsLock { on: true },
            ]
        );
        let undo: Vec<String> = changes.iter().filter_map(Change::undo).collect();
        assert_eq!(
            undo,
            ["switch the input language back to Deutsch (Deutschland)", "switch Filter Keys off", "switch Caps Lock off"]
        );
    }

    #[test]
    fn a_closed_window_is_told_by_its_handle_not_by_its_title() {
        let mut after = desk();
        after.windows[0].1 = "Angebot final.docx - Word".into(); // renamed, still open
        after.windows.remove(1);
        assert_eq!(diff(&desk(), &after), [Change::WindowClosed { title: "Posteingang - Outlook".into() }]);
    }

    #[test]
    fn unknown_touchpad_state_is_not_a_change() {
        let mut after = desk();
        after.touchpad = None;
        assert_eq!(diff(&desk(), &after), []);
        after.touchpad = Some(false);
        let changes = diff(&desk(), &after);
        assert_eq!(changes, [Change::Touchpad { on: false }]);
        assert!(changes[0].undo().is_some());
        assert_eq!(Change::Touchpad { on: true }.undo(), None);
    }
}
