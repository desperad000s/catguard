//! Tells a paw from a hand using only which keys are down and when they went
//! down. No character ever reaches this module.
//!
//! A finger presses one key. A paw is two key units wide and presses whatever
//! is under it in the same instant. The four rules below are four views of
//! that difference, ordered by how fast they decide.

use crate::layout::{extent, is_modifier, position, KeyCode, Pos};

/// Microseconds since an arbitrary start.
pub type Micros = u64;

const MS: Micros = 1_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// Three keys under one paw went down together. Decides on the third key.
    Slam,
    /// Four keys are down inside one paw-and-a-half. Decides on the fourth key.
    Chord,
    /// Two neighbours went down together and stayed down. Laptop keyboards
    /// often cannot report a third key in the same block, so this is the rule
    /// that catches a single paw there.
    Pair,
    /// Three keys held for seconds: the cat lies on the keyboard.
    Sit,
}

#[derive(Clone, Debug)]
pub struct Thresholds {
    /// Slam across two rows. Any such triple inside `slam_width` contains two
    /// keys of the same finger column, which one finger cannot press this
    /// fast, so the window can be generous.
    pub slam_window: Micros,
    pub slam_width: f32,
    /// Slam within one row. Fast typists roll "wer" or "asd", so the window
    /// must stay below their fastest roll.
    pub slam_row_window: Micros,
    pub slam_row_width: f32,

    pub chord_keys: usize,
    pub chord_height: f32,
    pub chord_width: f32,

    pub pair_gap: Micros,
    pub pair_hold: Micros,
    pub pair_width: f32,

    pub sit_keys: usize,
    pub sit_hold: Micros,

    /// A key-up can get lost (UAC prompt, Win+L). Forget keys after this long
    /// so that one lost event cannot count towards a rule forever.
    pub stale_after: Micros,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            slam_window: 60 * MS,
            slam_width: 1.5,
            slam_row_window: 25 * MS,
            slam_row_width: 2.0,
            chord_keys: 4,
            chord_height: 2.0,
            chord_width: 3.5,
            pair_gap: 30 * MS,
            pair_hold: 250 * MS,
            pair_width: 1.25,
            sit_keys: 3,
            sit_hold: 2_000 * MS,
            stale_after: 10_000 * MS,
        }
    }
}

struct Held {
    key: KeyCode,
    since: Micros,
    pos: Option<Pos>,
}

pub struct Detector {
    thresholds: Thresholds,
    down: Vec<Held>,
    hit_since: Micros,
}

impl Detector {
    pub fn new(thresholds: Thresholds) -> Self {
        Self { thresholds, down: Vec::with_capacity(16), hit_since: 0 }
    }

    /// When the first key of the pattern that fired last went down.
    pub fn hit_since(&self) -> Micros {
        self.hit_since
    }

    /// True when no key is tracked, so nobody needs to call [`Self::poll`].
    pub fn is_idle(&self) -> bool {
        self.down.is_empty()
    }

    pub fn clear(&mut self) {
        self.down.clear();
    }

    pub fn key_up(&mut self, key: KeyCode) {
        self.down.retain(|h| h.key != key);
    }

    /// Records a key going down and runs the rules that decide on the spot.
    /// Auto-repeat of a key that is already down changes nothing.
    pub fn key_down(&mut self, key: KeyCode, now: Micros) -> Option<Rule> {
        if is_modifier(key) || self.down.iter().any(|h| h.key == key) {
            return None;
        }
        self.forget_stale(now);
        self.down.push(Held { key, since: now, pos: position(key) });
        let hit = self.slam(now).map(|t| (Rule::Slam, t)).or_else(|| self.chord().map(|t| (Rule::Chord, t)));
        self.note(hit)
    }

    /// Runs the rules that need time to pass. Call every few tens of
    /// milliseconds while [`Self::is_idle`] is false.
    pub fn poll(&mut self, now: Micros) -> Option<Rule> {
        self.forget_stale(now);
        let hit = self.pair(now).map(|t| (Rule::Pair, t)).or_else(|| self.sit(now).map(|t| (Rule::Sit, t)));
        self.note(hit)
    }

    fn note(&mut self, hit: Option<(Rule, Micros)>) -> Option<Rule> {
        let (rule, since) = hit?;
        self.hit_since = since;
        Some(rule)
    }

    fn forget_stale(&mut self, now: Micros) {
        let limit = self.thresholds.stale_after;
        self.down.retain(|h| now.saturating_sub(h.since) < limit);
    }

    fn placed(&self) -> impl Iterator<Item = (Micros, Pos)> + '_ {
        self.down.iter().filter_map(|h| h.pos.map(|p| (h.since, p)))
    }

    /// The newest key plus two others, all down within the window, all under
    /// one paw.
    fn slam(&self, now: Micros) -> Option<Micros> {
        let t = &self.thresholds;
        let newest = self.down.last()?.pos?;
        let recent: Vec<(Micros, Pos)> = self
            .placed()
            .filter(|(since, _)| now - since <= t.slam_window)
            .collect();
        let others = &recent[..recent.len() - 1];
        for (i, a) in others.iter().enumerate() {
            for b in &others[i + 1..] {
                let age = now - a.0.min(b.0);
                let (height, width) = extent(&[a.1, b.1, newest]);
                let under_one_paw = if height == 0.0 {
                    age <= t.slam_row_window && width <= t.slam_row_width
                } else {
                    height <= 1.0 && width <= t.slam_width
                };
                if under_one_paw {
                    return Some(now - age);
                }
            }
        }
        None
    }

    fn chord(&self) -> Option<Micros> {
        let t = &self.thresholds;
        let newest = self.down.last()?.pos?;
        let near: Vec<(Micros, Pos)> = self
            .placed()
            .filter(|(_, p)| {
                let (height, width) = extent(&[*p, newest]);
                height <= t.chord_height && width <= t.chord_width
            })
            .collect();
        let places: Vec<Pos> = near.iter().map(|(_, p)| *p).collect();
        let (height, width) = extent(&places);
        let fits = near.len() >= t.chord_keys && height <= t.chord_height && width <= t.chord_width;
        fits.then(|| near.iter().map(|(since, _)| *since).min().unwrap_or(0))
    }

    fn pair(&self, now: Micros) -> Option<Micros> {
        let t = &self.thresholds;
        let placed: Vec<(Micros, Pos)> = self.placed().collect();
        for (i, a) in placed.iter().enumerate() {
            for b in &placed[i + 1..] {
                let together = a.0.abs_diff(b.0) <= t.pair_gap;
                let held = now - a.0.max(b.0) >= t.pair_hold;
                let (height, width) = extent(&[a.1, b.1]);
                if together && held && height <= 1.0 && width <= t.pair_width {
                    return Some(a.0.min(b.0));
                }
            }
        }
        None
    }

    fn sit(&self, now: Micros) -> Option<Micros> {
        let t = &self.thresholds;
        let long_held = self.down.iter().filter(|h| now - h.since >= t.sit_hold);
        (long_held.clone().count() >= t.sit_keys).then(|| long_held.map(|h| h.since).min().unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::key_for;

    fn detector() -> Detector {
        Detector::new(Thresholds::default())
    }

    /// Presses each letter `gap_ms` after the previous one and holds them all.
    fn press_all(d: &mut Detector, keys: &str, gap_ms: u64) -> Option<Rule> {
        let mut fired = None;
        for (i, c) in keys.chars().enumerate() {
            fired = fired.or(d.key_down(key_for(c), i as u64 * gap_ms * MS));
        }
        fired
    }

    #[test]
    fn paw_on_three_keys_fires_on_the_third() {
        let mut d = detector();
        assert_eq!(d.key_down(key_for('w'), 0), None);
        assert_eq!(d.key_down(key_for('e'), 4 * MS), None);
        assert_eq!(d.key_down(key_for('d'), 9 * MS), Some(Rule::Slam));
    }

    #[test]
    fn paw_on_the_front_edge_counts_the_space_bar() {
        assert_eq!(press_all(&mut detector(), "v b", 5), Some(Rule::Slam));
    }

    #[test]
    fn fast_rolls_are_human() {
        // 20 ms between keys is a burst no typist sustains. It must pass.
        for word in ["wer", "asd", "oiu", "awe", "ion"] {
            assert_eq!(press_all(&mut detector(), word, 20), None, "{word}");
        }
    }

    #[test]
    fn same_finger_words_are_too_slow_to_look_like_a_paw() {
        // "was" sits under one paw, but W and S share the ring finger, and a
        // finger needs about 100 ms to reach its next key.
        let mut d = detector();
        assert_eq!(d.key_down(key_for('w'), 0), None);
        assert_eq!(d.key_down(key_for('a'), 30 * MS), None);
        assert_eq!(d.key_down(key_for('s'), 120 * MS), None);
    }

    #[test]
    fn same_row_slam_needs_a_tighter_window() {
        assert_eq!(press_all(&mut detector(), "sdf", 10), Some(Rule::Slam));
        assert_eq!(press_all(&mut detector(), "sdf", 15), None);
    }

    #[test]
    fn four_keys_in_one_spot_is_a_chord_however_slowly_they_arrive() {
        assert_eq!(press_all(&mut detector(), "erdf", 200), Some(Rule::Chord));
    }

    #[test]
    fn four_keys_spread_over_the_board_is_not_a_chord() {
        assert_eq!(press_all(&mut detector(), "qpzm", 200), None);
        // The usual game hand: move, jump, reload.
        assert_eq!(press_all(&mut detector(), "wd r", 200), None);
    }

    #[test]
    fn two_neighbours_pressed_together_and_held_is_a_paw() {
        let mut d = detector();
        d.key_down(key_for('s'), 0);
        d.key_down(key_for('d'), 6 * MS);
        assert_eq!(d.poll(200 * MS), None);
        assert_eq!(d.poll(260 * MS), Some(Rule::Pair));
    }

    #[test]
    fn rollover_typing_releases_before_the_pair_rule_looks() {
        let mut d = detector();
        d.key_down(key_for('a'), 0);
        d.key_down(key_for('s'), 25 * MS);
        d.key_up(key_for('a'));
        assert_eq!(d.poll(400 * MS), None);
    }

    #[test]
    fn holding_w_then_tapping_d_is_a_game_not_a_paw() {
        let mut d = detector();
        d.key_down(key_for('w'), 0);
        d.key_down(key_for('d'), 500 * MS);
        assert_eq!(d.poll(900 * MS), None);
    }

    #[test]
    fn three_keys_held_for_seconds_is_a_sitting_cat() {
        let mut d = detector();
        press_all(&mut d, "q7m", 300);
        assert_eq!(d.poll(1_900 * MS), None);
        assert_eq!(d.poll(2_700 * MS), Some(Rule::Sit));
    }

    #[test]
    fn modifiers_and_auto_repeat_do_not_count() {
        let mut d = detector();
        for key in [0x1D, 0x2A, 0x38, 0xE05B] {
            assert_eq!(d.key_down(key, 0), None);
        }
        for i in 0..50 {
            assert_eq!(d.key_down(key_for('a'), i * 30 * MS), None);
        }
        assert_eq!(d.poll(1_600 * MS), None);
    }

    #[test]
    fn a_lost_key_up_is_forgotten() {
        let mut d = detector();
        d.key_down(key_for('l'), 0);
        assert_eq!(d.poll(11_000 * MS), None);
        assert!(d.is_idle());
    }
}
