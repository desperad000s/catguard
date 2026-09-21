//! What happened around a lock: which keys went down, for how long, which of
//! them reached the applications, and what can be done about those.
//!
//! The history lives in memory only, covers the last three seconds, and is
//! never written anywhere. It exists so that the human can see what the cat
//! did and take back the part that can be taken back.
//!
//! What can be taken back is narrow on purpose. catguard sees keys, not what
//! a program did with them. So it only offers two things: pressing a
//! shortcut again when the shortcut is its own opposite (Caps Lock, mute,
//! Win+D), and Backspace for characters that were typed with no modifier.

use std::collections::VecDeque;

use crate::detector::{Micros, Rule};
use crate::layout::{is_printable, KeyCode};

/// How far back the timeline reaches.
pub const LOOKBACK: Micros = 3_000_000;

const CAPACITY: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { ctrl: false, alt: false, shift: false, win: false };
    pub const CTRL: Mods = Mods { ctrl: true, ..Mods::NONE };
    pub const ALT: Mods = Mods { alt: true, ..Mods::NONE };
    pub const WIN: Mods = Mods { win: true, ..Mods::NONE };
    pub const CTRL_WIN: Mods = Mods { ctrl: true, win: true, ..Mods::NONE };
    pub const SHIFT_WIN: Mods = Mods { shift: true, win: true, ..Mods::NONE };

    /// True when the key produces a character rather than a command.
    fn types_text(self) -> bool {
        !self.ctrl && !self.alt && !self.win
    }
}

/// The modifier a key stands for, as a `Mods` with that one flag set.
fn modifier_of(key: KeyCode) -> Option<Mods> {
    match key {
        0x1D | 0xE01D => Some(Mods::CTRL),
        0x38 | 0xE038 => Some(Mods::ALT),
        0x2A | 0x36 => Some(Mods { shift: true, ..Mods::NONE }),
        0xE05B | 0xE05C => Some(Mods::WIN),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Press {
    pub key: KeyCode,
    pub vk: u8,
    /// Modifiers that were down when this key went down.
    pub mods: Mods,
    pub down: Micros,
    pub up: Option<Micros>,
    /// False when catguard swallowed the key-down.
    pub passed: bool,
    /// Auto-repeats that reached the applications after the first key-down.
    pub repeats: u32,
}

#[derive(Default)]
pub struct History {
    presses: VecDeque<Press>,
    held_mods: Vec<KeyCode>,
    lock: Option<(Rule, Micros, Micros)>,
}

impl History {
    pub fn key_down(&mut self, key: KeyCode, vk: u8, now: Micros, passed: bool, repeat: bool) {
        if repeat {
            if let Some(open) = self.presses.iter_mut().rev().find(|p| p.key == key && p.up.is_none()) {
                open.repeats += u32::from(passed);
                return;
            }
        }
        let mods = self.held_mods.iter().filter_map(|&k| modifier_of(k)).fold(Mods::NONE, |a, b| Mods {
            ctrl: a.ctrl || b.ctrl,
            alt: a.alt || b.alt,
            shift: a.shift || b.shift,
            win: a.win || b.win,
        });
        if modifier_of(key).is_some() && passed && !self.held_mods.contains(&key) {
            self.held_mods.push(key);
        }
        if self.presses.len() == CAPACITY {
            self.presses.pop_front();
        }
        self.presses.push_back(Press { key, vk, mods, down: now, up: None, passed, repeats: 0 });
    }

    pub fn key_up(&mut self, key: KeyCode, now: Micros) {
        self.held_mods.retain(|&k| k != key);
        if let Some(open) = self.presses.iter_mut().rev().find(|p| p.key == key && p.up.is_none()) {
            open.up = Some(now);
        }
    }

    /// Drops what is older than the timeline shows. Not called while locked,
    /// so that an incident stays whole however long the cat stays.
    pub fn prune(&mut self, now: Micros) {
        while let Some(oldest) = self.presses.front() {
            if now.saturating_sub(oldest.up.unwrap_or(now)) > LOOKBACK {
                self.presses.pop_front();
            } else {
                break;
            }
        }
    }

    /// `paw_since` is when the first key of the pattern that fired went down.
    pub fn mark_lock(&mut self, rule: Rule, now: Micros, paw_since: Micros) {
        self.prune(now);
        self.lock = Some((rule, now, paw_since));
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn incident(&self, now: Micros) -> Option<Incident> {
        let (rule, locked_at, paw_since) = self.lock?;
        Some(Incident {
            rule,
            paw_since,
            locked_at,
            taken_at: now,
            presses: self.presses.iter().cloned().collect(),
        })
    }
}

/// A snapshot of the history around one lock.
#[derive(Clone, Debug)]
pub struct Incident {
    pub rule: Rule,
    pub paw_since: Micros,
    pub locked_at: Micros,
    pub taken_at: Micros,
    pub presses: Vec<Press>,
}

/// One key combination that reached the applications.
#[derive(Clone, Debug, PartialEq)]
pub struct Leak {
    pub mods: Mods,
    pub vk: u8,
    pub key: KeyCode,
    /// Key-downs including auto-repeat.
    pub count: u32,
    pub note: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UndoStep {
    /// Press this combination once.
    Press(Mods, u8),
    Backspace(u32),
}

impl Incident {
    /// While the paw was down: between the first key of the pattern and the
    /// lock. Characters are only blamed on the cat inside this window.
    pub fn during_paw(&self, p: &Press) -> bool {
        p.passed && p.down <= self.locked_at && p.up.unwrap_or(self.locked_at) >= self.paw_since
    }

    /// The whole timeline before the lock. A cat that walks onto the keyboard
    /// hits single keys before the step that gets it caught, so switches are
    /// looked for here. A human rarely flips Caps Lock in the same three
    /// seconds, and the undo button says what it will press.
    fn before_lock(&self, p: &Press) -> bool {
        p.passed && p.down <= self.locked_at
    }

    /// Everything that got through, grouped by combination, in order of
    /// first appearance.
    pub fn leaks(&self) -> Vec<Leak> {
        let mut leaks: Vec<Leak> = Vec::new();
        for p in &self.presses {
            if modifier_of(p.key).is_some() {
                continue;
            }
            let known = shortcut(p.mods, p.vk);
            let relevant = self.during_paw(p) || (self.before_lock(p) && known.as_ref().is_some_and(|s| s.undo.is_some()));
            if !relevant {
                continue;
            }
            let count = 1 + p.repeats;
            match leaks.iter_mut().find(|l| l.mods == p.mods && l.vk == p.vk) {
                Some(leak) => leak.count += count,
                None => leaks.push(Leak { mods: p.mods, vk: p.vk, key: p.key, count, note: known.map(|s| s.note) }),
            }
        }
        leaks
    }

    /// The steps that take back what can be taken back. Empty when nothing can.
    pub fn undo_plan(&self) -> Vec<UndoStep> {
        let mut steps = Vec::new();
        let mut typed = 0;
        let mut only_text = true;
        for p in self.presses.iter().filter(|p| modifier_of(p.key).is_none()) {
            let undo = shortcut(p.mods, p.vk).and_then(|s| s.undo);
            if let (Some(undo), true) = (undo, self.before_lock(p)) {
                // Auto-repeat does not flip a switch again, a second press does.
                let step = match undo {
                    Undo::PressAgain => UndoStep::Press(p.mods, p.vk),
                    Undo::PressInstead(mods, vk) => UndoStep::Press(mods, vk),
                };
                match (undo, steps.iter().position(|s| *s == step)) {
                    (Undo::PressAgain, Some(i)) => {
                        steps.remove(i);
                    }
                    (Undo::PressInstead(..), Some(_)) => {}
                    (_, None) => steps.push(step),
                }
            } else if self.during_paw(p) {
                if p.mods.types_text() && is_printable(p.key) {
                    typed += 1 + p.repeats;
                } else {
                    only_text = false;
                }
            }
        }
        // Backspace is only right when characters are all that arrived. After
        // an Enter or a Ctrl+W the caret is somewhere else.
        if typed > 0 && only_text {
            steps.push(UndoStep::Backspace(typed));
        }
        steps
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Undo {
    PressAgain,
    PressInstead(Mods, u8),
}

struct Shortcut {
    note: &'static str,
    undo: Option<Undo>,
}

// Virtual-key codes used below.
const VK_BACK: u8 = 0x08;
const VK_TAB: u8 = 0x09;
const VK_RETURN: u8 = 0x0D;
const VK_CAPITAL: u8 = 0x14;
const VK_ESCAPE: u8 = 0x1B;
const VK_INSERT: u8 = 0x2D;
const VK_DELETE: u8 = 0x2E;
const VK_LWIN: u8 = 0x5B;
const VK_F4: u8 = 0x73;
const VK_F5: u8 = 0x74;
const VK_F11: u8 = 0x7A;
const VK_F24: u8 = 0x87;
const VK_NUMLOCK: u8 = 0x90;
const VK_SCROLL: u8 = 0x91;
const VK_VOLUME_MUTE: u8 = 0xAD;
const VK_VOLUME_DOWN: u8 = 0xAE;
const VK_VOLUME_UP: u8 = 0xAF;
const VK_MEDIA_PLAY_PAUSE: u8 = 0xB3;

/// What a combination does in Windows or in most programs, and whether
/// pressing keys can take it back. Shift is ignored for the lookup.
fn shortcut(mods: Mods, vk: u8) -> Option<Shortcut> {
    let again = Some(Undo::PressAgain);
    let base = Mods { shift: false, ..mods };
    let (note, undo) = match (base, vk) {
        (Mods::NONE, VK_CAPITAL) => ("switched Caps Lock", again),
        (Mods::NONE, VK_NUMLOCK) => ("switched Num Lock", again),
        (Mods::NONE, VK_SCROLL) => ("switched Scroll Lock", again),
        (Mods::NONE, VK_INSERT) => ("switched between insert and overwrite in editors", again),
        (Mods::NONE, VK_VOLUME_MUTE) => ("switched the sound on or off", again),
        (Mods::NONE, VK_MEDIA_PLAY_PAUSE) => ("started or paused playback", again),
        (Mods::NONE, VK_VOLUME_UP) => ("turned the volume up", None),
        (Mods::NONE, VK_VOLUME_DOWN) => ("turned the volume down", None),
        // What the Fn key for the touchpad sends on laptops with a precision
        // touchpad. Fn itself never reaches Windows.
        (Mods::CTRL_WIN, VK_F24) => ("switched the touchpad on or off", again),
        (Mods::CTRL_WIN, b'C') => ("switched the colour filter, if its shortcut is enabled", again),
        (Mods::CTRL_WIN, VK_RETURN) => ("started or stopped Narrator", again),
        (Mods::WIN, b'D') => ("showed or hid the desktop", again),
        (Mods::WIN, b'M') if !mods.shift => {
            ("minimized all windows", Some(Undo::PressInstead(Mods::SHIFT_WIN, b'M')))
        }
        (Mods::WIN, b'L') => ("locked the PC", None),
        (Mods::WIN, b'E') => ("opened Explorer", None),
        (Mods::WIN, b'R') => ("opened the Run box", None),
        (Mods::WIN, b'P') => ("opened the projection menu; check the display mode", None),
        (Mods::NONE, VK_LWIN) => ("opened the Start menu", None),
        (Mods::ALT, VK_F4) => ("closed a window", None),
        (Mods::ALT, VK_TAB) => ("switched to another window", None),
        (Mods::CTRL, b'W') => ("closed a tab or document; browsers reopen it with Ctrl+Shift+T", None),
        (Mods::CTRL, b'S') => ("saved the document as it was", None),
        (Mods::CTRL, b'Z') => ("undid your last step; most programs redo it with Ctrl+Y", None),
        (Mods::CTRL, b'V') => ("pasted the clipboard; Ctrl+Z usually takes it back", None),
        (Mods::CTRL, b'X') => ("cut the selection; Ctrl+Z usually takes it back", None),
        (Mods::CTRL, b'A') => ("selected everything; the next key typed replaced it", None),
        (Mods::NONE, VK_DELETE) => ("deleted what was selected; Ctrl+Z usually takes it back", None),
        (Mods::NONE, VK_BACK) => ("deleted a character or went back a page", None),
        (Mods::NONE, VK_RETURN) => ("confirmed a dialog or sent a message", None),
        (Mods::NONE, VK_ESCAPE) => ("cancelled a dialog", None),
        (Mods::NONE, VK_F5) => ("reloaded the page", None),
        (Mods::NONE, VK_F11) => ("switched full screen in most programs; F11 switches back", None),
        _ => return None,
    };
    Some(Shortcut { note, undo })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::key_for;

    const MS: Micros = 1_000;
    const CTRL: KeyCode = 0x1D;
    const WIN: KeyCode = 0xE05B;
    const CAPS: (KeyCode, u8) = (0x3A, VK_CAPITAL);

    fn letter(c: char) -> (KeyCode, u8) {
        (key_for(c), c.to_ascii_uppercase() as u8)
    }

    /// Key down at `ms`, up 50 ms later.
    fn tap(h: &mut History, (key, vk): (KeyCode, u8), ms: u64, passed: bool) {
        h.key_down(key, vk, ms * MS, passed, false);
        h.key_up(key, (ms + 50) * MS);
    }

    /// W and E get through, D is swallowed, the lock falls at `ms + 6`.
    fn slam(h: &mut History, ms: u64) -> Incident {
        let ((w, w_vk), (e, e_vk), (d, d_vk)) = (letter('w'), letter('e'), letter('d'));
        h.key_down(w, w_vk, ms * MS, true, false);
        h.key_down(e, e_vk, (ms + 3) * MS, true, false);
        h.key_down(d, d_vk, (ms + 6) * MS, false, false);
        h.mark_lock(Rule::Slam, (ms + 6) * MS, ms * MS);
        h.incident((ms + 100) * MS).unwrap()
    }

    #[test]
    fn a_slam_leaks_two_characters_and_two_backspaces_take_them_back() {
        let incident = slam(&mut History::default(), 1_000);
        let leaked: Vec<u8> = incident.leaks().iter().map(|l| l.vk).collect();
        assert_eq!(leaked, [b'W', b'E']);
        assert_eq!(incident.undo_plan(), [UndoStep::Backspace(2)]);
    }

    #[test]
    fn what_the_human_typed_before_the_paw_is_shown_but_not_blamed_on_the_cat() {
        let mut h = History::default();
        tap(&mut h, letter('h'), 0, true);
        tap(&mut h, letter('i'), 200, true);
        let incident = slam(&mut h, 1_000);
        assert_eq!(incident.presses.len(), 5);
        assert_eq!(incident.undo_plan(), [UndoStep::Backspace(2)]);
    }

    #[test]
    fn auto_repeat_counts_as_typed_characters() {
        let mut h = History::default();
        let (g, vk) = letter('g');
        h.key_down(g, vk, 0, true, false);
        for i in 1..=10 {
            h.key_down(g, vk, (500 + i * 33) * MS, true, true);
        }
        h.mark_lock(Rule::Sit, 2_000 * MS, 0);
        let incident = h.incident(2_000 * MS).unwrap();
        assert_eq!(incident.leaks()[0].count, 11);
        assert_eq!(incident.undo_plan(), [UndoStep::Backspace(11)]);
    }

    #[test]
    fn a_command_among_the_characters_rules_out_backspace() {
        let mut h = History::default();
        h.key_down(CTRL, 0x11, 990 * MS, true, false);
        let (w, vk) = letter('w');
        h.key_down(w, vk, 1_000 * MS, true, false);
        h.key_down(key_for('e'), b'E', 1_003 * MS, true, false);
        h.mark_lock(Rule::Slam, 1_006 * MS, 1_000 * MS);
        let incident = h.incident(1_100 * MS).unwrap();

        let leaks = incident.leaks();
        assert_eq!(leaks[0].mods, Mods::CTRL);
        assert!(leaks[0].note.unwrap().contains("closed a tab"));
        assert_eq!(incident.undo_plan(), []);
    }

    #[test]
    fn caps_lock_from_an_earlier_pawstep_is_pressed_again() {
        let mut h = History::default();
        tap(&mut h, CAPS, 100, true);
        let incident = slam(&mut h, 1_000);
        assert_eq!(
            incident.undo_plan(),
            [UndoStep::Press(Mods::NONE, VK_CAPITAL), UndoStep::Backspace(2)]
        );
    }

    #[test]
    fn a_switch_flipped_twice_needs_no_undo() {
        let mut h = History::default();
        tap(&mut h, CAPS, 100, true);
        tap(&mut h, CAPS, 400, true);
        assert_eq!(slam(&mut h, 1_000).undo_plan(), [UndoStep::Backspace(2)]);
    }

    #[test]
    fn the_touchpad_key_is_recognized_and_reversible() {
        let mut h = History::default();
        h.key_down(CTRL, 0x11, 500 * MS, true, false);
        h.key_down(WIN, VK_LWIN, 500 * MS, true, false);
        h.key_down(0x76, VK_F24, 501 * MS, true, false);
        h.key_up(0x76, 560 * MS);
        h.key_up(WIN, 560 * MS);
        h.key_up(CTRL, 560 * MS);
        let incident = slam(&mut h, 1_000);
        assert_eq!(incident.leaks()[0].note, Some("switched the touchpad on or off"));
        assert_eq!(incident.undo_plan()[0], UndoStep::Press(Mods::CTRL_WIN, VK_F24));
    }

    #[test]
    fn minimize_all_is_undone_with_its_counterpart() {
        let mut h = History::default();
        h.key_down(WIN, VK_LWIN, 500 * MS, true, false);
        tap(&mut h, letter('m'), 510, true);
        h.key_up(WIN, 600 * MS);
        assert_eq!(slam(&mut h, 1_000).undo_plan()[0], UndoStep::Press(Mods::SHIFT_WIN, b'M'));
    }

    #[test]
    fn keys_swallowed_during_the_lock_are_recorded_but_never_leak() {
        let mut h = History::default();
        slam(&mut h, 1_000);
        tap(&mut h, letter('x'), 1_500, false);
        let incident = h.incident(2_000 * MS).unwrap();
        assert_eq!(incident.presses.len(), 4);
        assert_eq!(incident.leaks().len(), 2);
    }

    #[test]
    fn the_history_forgets_after_three_seconds() {
        let mut h = History::default();
        tap(&mut h, letter('p'), 0, true);
        h.prune(2_000 * MS);
        assert_eq!(h.presses.len(), 1);
        h.prune(4_000 * MS);
        assert!(h.presses.is_empty());
    }
}
