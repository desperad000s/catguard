//! Decides for every key event whether it reaches the applications.
//!
//! The rule that keeps applications sane: a key-up is delivered exactly when
//! its key-down was. Swallowing only one half leaves a key stuck.

use crate::detector::{Detector, Micros, Rule, Thresholds};
use crate::history::{History, Incident};
use crate::layout::KeyCode;

/// The default unlock word. See [`Guard::set_unlock_word`].
pub const UNLOCK_WORD: &str = "human";

/// Minimum pause between two deterrent sounds.
const DETER_GAP: Micros = 1_500_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Show the lock window and play the sound.
    Lock(Rule),
    /// Another pawstep while locked: play the sound again.
    Deter,
    /// So many letters of the unlock word have been typed.
    Progress(usize),
    Unlock,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub swallow: bool,
    pub action: Option<Action>,
}

const PASS: Verdict = Verdict { swallow: false, action: None };
const SWALLOW: Verdict = Verdict { swallow: true, action: None };

pub struct Guard {
    detector: Detector,
    locked: bool,
    /// Keys whose key-down reached the applications and that are still down.
    passed: Vec<KeyCode>,
    /// Keys whose key-down was swallowed and that are still down. They stay
    /// swallowed after an unlock, until the cat lets go.
    swallowed: Vec<KeyCode>,
    /// Upper-case ASCII, which is what virtual-key codes are for letters on
    /// every layout.
    word: Vec<u8>,
    typed: Vec<u8>,
    last_sound: Micros,
    history: History,
}

impl Guard {
    pub fn new(thresholds: Thresholds) -> Self {
        Self {
            detector: Detector::new(thresholds),
            locked: false,
            passed: Vec::with_capacity(16),
            swallowed: Vec::with_capacity(16),
            word: UNLOCK_WORD.to_ascii_uppercase().into_bytes(),
            typed: Vec::new(),
            last_sound: 0,
            history: History::default(),
        }
    }

    pub fn set_thresholds(&mut self, thresholds: Thresholds) {
        self.detector.set_thresholds(thresholds);
    }

    /// The word that unlocks, typed blind. Expects ASCII letters; see
    /// `Settings::sanitized`.
    pub fn set_unlock_word(&mut self, word: &str) {
        self.word = word.to_ascii_uppercase().into_bytes();
        self.typed.clear();
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// True when [`Self::poll`] has nothing to look at.
    pub fn is_idle(&self) -> bool {
        self.detector.is_idle()
    }

    /// Forgets everything. Used while the guard is paused.
    pub fn reset(&mut self) {
        self.detector.clear();
        self.locked = false;
        self.passed.clear();
        self.swallowed.clear();
        self.typed.clear();
        self.history.clear();
    }

    /// The unlock button was clicked.
    pub fn unlock(&mut self) {
        self.locked = false;
        self.typed.clear();
    }

    /// The keys around the last lock, or `None` before the first one.
    pub fn incident(&self, now: Micros) -> Option<Incident> {
        self.history.incident(now)
    }

    pub fn key_down(&mut self, key: KeyCode, vk: u8, now: Micros) -> Verdict {
        let repeat = self.passed.contains(&key) || self.swallowed.contains(&key);
        let verdict = self.decide_down(key, vk, now);
        self.history.key_down(key, vk, now, !verdict.swallow, repeat);
        if !self.locked {
            self.history.prune(now);
        }
        verdict
    }

    fn decide_down(&mut self, key: KeyCode, vk: u8, now: Micros) -> Verdict {
        if self.passed.contains(&key) {
            // Auto-repeat of a key the applications already know is down.
            return if self.locked { SWALLOW } else { PASS };
        }
        if self.swallowed.contains(&key) {
            return SWALLOW;
        }

        let fired = self.detector.key_down(key, now);
        if !self.locked {
            return match fired {
                Some(rule) => {
                    self.swallowed.push(key);
                    self.lock(rule, now)
                }
                None => {
                    self.passed.push(key);
                    PASS
                }
            };
        }

        self.swallowed.push(key);
        if self.typed.len() == self.word.len() {
            self.typed.remove(0);
        }
        self.typed.push(vk);
        let progress = (1..=self.typed.len())
            .rev()
            .find(|&n| self.typed.ends_with(&self.word[..n]))
            .unwrap_or(0);

        let action = if progress == self.word.len() {
            self.unlock();
            Action::Unlock
        } else if fired.is_some() && now - self.last_sound >= DETER_GAP {
            self.last_sound = now;
            Action::Deter
        } else {
            Action::Progress(progress)
        };
        Verdict { swallow: true, action: Some(action) }
    }

    pub fn key_up(&mut self, key: KeyCode, now: Micros) -> Verdict {
        self.detector.key_up(key);
        self.history.key_up(key, now);
        if let Some(i) = self.passed.iter().position(|&k| k == key) {
            self.passed.swap_remove(i);
            return PASS;
        }
        if let Some(i) = self.swallowed.iter().position(|&k| k == key) {
            self.swallowed.swap_remove(i);
            return SWALLOW;
        }
        PASS
    }

    /// Runs the rules that need time to pass.
    pub fn poll(&mut self, now: Micros) -> Option<Action> {
        let fired = self.detector.poll(now);
        if self.locked {
            return None;
        }
        fired.and_then(|rule| self.lock(rule, now).action)
    }

    fn lock(&mut self, rule: Rule, now: Micros) -> Verdict {
        self.locked = true;
        self.typed.clear();
        self.last_sound = now;
        self.history.mark_lock(rule, now, self.detector.hit_since());
        Verdict { swallow: true, action: Some(Action::Lock(rule)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::key_for;

    const MS: Micros = 1_000;

    fn guard() -> Guard {
        Guard::new(Thresholds::default())
    }

    fn down(g: &mut Guard, c: char, ms: u64) -> Verdict {
        g.key_down(key_for(c), c.to_ascii_uppercase() as u8, ms * MS)
    }

    fn up(g: &mut Guard, c: char) -> Verdict {
        g.key_up(key_for(c), 0)
    }

    fn slam(g: &mut Guard, ms: u64) -> Verdict {
        down(g, 'w', ms);
        down(g, 'e', ms + 3);
        down(g, 'd', ms + 6)
    }

    /// Types a word the way a person does: one key at a time, 90 ms down.
    fn type_word(g: &mut Guard, word: &str, start_ms: u64) -> Vec<Verdict> {
        let mut verdicts = Vec::new();
        for (i, c) in word.chars().enumerate() {
            verdicts.push(down(g, c, start_ms + i as u64 * 150));
            up(g, c);
        }
        verdicts
    }

    #[test]
    fn a_sentence_passes_untouched() {
        let mut g = guard();
        let text = "the quick brown fox jumps over the lazy dog";
        // 60 ms between keys, each released only after the next one is down.
        let chars: Vec<char> = text.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            if i > 0 && chars[i - 1] != c {
                assert_eq!(down(&mut g, c, i as u64 * 60), PASS, "{c:?} at {i}");
                assert_eq!(up(&mut g, chars[i - 1]), PASS);
            } else if i > 0 {
                assert_eq!(up(&mut g, c), PASS);
                assert_eq!(down(&mut g, c, i as u64 * 60), PASS);
            } else {
                assert_eq!(down(&mut g, c, 0), PASS);
            }
            assert_eq!(g.poll(i as u64 * 60 * MS + 30 * MS), None);
        }
        assert!(!g.is_locked());
    }

    #[test]
    fn a_paw_locks_and_no_key_is_left_stuck() {
        let mut g = guard();
        let third = slam(&mut g, 0);
        assert_eq!(third, Verdict { swallow: true, action: Some(Action::Lock(Rule::Slam)) });

        // W and E reached the applications, so their key-ups must too.
        assert_eq!(up(&mut g, 'w'), PASS);
        assert_eq!(up(&mut g, 'e'), PASS);
        // D never arrived, so neither does its key-up.
        assert_eq!(up(&mut g, 'd'), SWALLOW);
    }

    #[test]
    fn auto_repeat_of_a_key_under_the_paw_stops_at_the_lock() {
        let mut g = guard();
        assert_eq!(down(&mut g, 'w', 0), PASS);
        assert_eq!(down(&mut g, 'w', 30), PASS);
        assert_eq!(down(&mut g, 'e', 40), PASS);
        assert_eq!(down(&mut g, 'd', 45).action, Some(Action::Lock(Rule::Slam)));
        assert_eq!(down(&mut g, 'w', 60), SWALLOW);
        assert_eq!(up(&mut g, 'w'), PASS);
    }

    #[test]
    fn everything_is_swallowed_while_locked() {
        let mut g = guard();
        slam(&mut g, 0);
        for v in type_word(&mut g, "rm", 1_000) {
            assert!(v.swallow);
        }
        assert!(g.is_locked());
    }

    #[test]
    fn typing_human_unlocks() {
        let mut g = guard();
        slam(&mut g, 0);
        for c in ['w', 'e', 'd'] {
            up(&mut g, c);
        }
        let verdicts = type_word(&mut g, "human", 1_000);
        let actions: Vec<Action> = verdicts.iter().map(|v| v.action.unwrap()).collect();
        assert_eq!(
            actions,
            [
                Action::Progress(1),
                Action::Progress(2),
                Action::Progress(3),
                Action::Progress(4),
                Action::Unlock
            ]
        );
        assert!(verdicts.iter().all(|v| v.swallow));
        assert!(!g.is_locked());
        assert_eq!(down(&mut g, 'x', 3_000), PASS);
    }

    #[test]
    fn a_typo_restarts_the_word_without_a_reset_key() {
        let mut g = guard();
        slam(&mut g, 0);
        type_word(&mut g, "huhuman", 1_000);
        assert!(!g.is_locked());
    }

    #[test]
    fn a_cat_lying_on_keys_does_not_stop_the_human_from_unlocking() {
        let mut g = guard();
        slam(&mut g, 0);
        for i in 0..20 {
            down(&mut g, 'd', 500 + i * 33);
        }
        type_word(&mut g, "human", 2_000);
        assert!(!g.is_locked());
    }

    #[test]
    fn a_key_still_under_the_cat_stays_dead_after_the_unlock() {
        let mut g = guard();
        slam(&mut g, 0);
        up(&mut g, 'w');
        up(&mut g, 'e');
        g.unlock();
        assert_eq!(down(&mut g, 'd', 5_000), SWALLOW);
        assert_eq!(up(&mut g, 'd'), SWALLOW);
        assert_eq!(down(&mut g, 'd', 6_000), PASS);
    }

    #[test]
    fn a_second_pawstep_sounds_again_but_not_in_a_burst() {
        let mut g = guard();
        slam(&mut g, 0);
        for c in ['w', 'e', 'd'] {
            up(&mut g, c);
        }
        assert_eq!(slam(&mut g, 500).action, Some(Action::Progress(0)));
        for c in ['w', 'e', 'd'] {
            up(&mut g, c);
        }
        assert_eq!(slam(&mut g, 2_000).action, Some(Action::Deter));
    }

    #[test]
    fn a_held_pair_locks_from_the_poll() {
        let mut g = guard();
        assert_eq!(down(&mut g, 's', 0), PASS);
        assert_eq!(down(&mut g, 'd', 5), PASS);
        assert_eq!(g.poll(300 * MS), Some(Action::Lock(Rule::Pair)));
        assert_eq!(g.poll(330 * MS), None);
        assert_eq!(down(&mut g, 's', 340), SWALLOW);
        assert_eq!(up(&mut g, 's'), PASS);
    }

    #[test]
    fn the_unlock_word_can_be_changed() {
        let mut g = guard();
        g.set_unlock_word("mensch");
        slam(&mut g, 0);
        for c in ['w', 'e', 'd'] {
            up(&mut g, c);
        }
        type_word(&mut g, "human", 1_000);
        assert!(g.is_locked());
        type_word(&mut g, "mensch", 3_000);
        assert!(!g.is_locked());
    }

    #[test]
    fn the_incident_knows_what_got_through() {
        use crate::history::UndoStep;
        let mut g = guard();
        assert!(g.incident(0).is_none());
        slam(&mut g, 1_000);
        let incident = g.incident(1_100 * MS).unwrap();
        assert_eq!(incident.rule, Rule::Slam);
        assert_eq!(incident.paw_since, 1_000 * MS);
        assert_eq!(incident.undo_plan(), [UndoStep::Backspace(2)]);
    }

    #[test]
    fn reset_forgets_the_lock() {
        let mut g = guard();
        slam(&mut g, 0);
        g.reset();
        assert!(!g.is_locked() && g.is_idle());
    }
}
