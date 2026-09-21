//! Physical key positions, in key units (one unit = one letter key, about 19 mm).
//!
//! Keys are identified by their set-1 scancode, so the geometry is the same
//! for QWERTY, QWERTZ and AZERTY: the scancode names the physical key, not the
//! character printed on it.

/// Set-1 scancode. Extended keys (arrows, right Ctrl, ...) carry [`EXTENDED`].
pub type KeyCode = u16;

pub const EXTENDED: KeyCode = 0xE000;

/// A key's place on the board. Rows count down from the function row (0) to
/// the space bar row (5). `half_width` is non-zero only for the space bar, so
/// that a paw on `V` and a paw on `M` are both "next to space".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pos {
    pub row: f32,
    pub x: f32,
    pub half_width: f32,
}

const fn at(row: f32, x: f32) -> Option<Pos> {
    Some(Pos { row, x, half_width: 0.0 })
}

/// Shift, Ctrl, Alt and Win. Humans hold these together with other keys all
/// the time, so no rule counts them.
pub fn is_modifier(key: KeyCode) -> bool {
    matches!(
        key,
        0x2A | 0x36 | 0x1D | 0x38 | 0xE02A | 0xE036 | 0xE01D | 0xE038 | 0xE05B | 0xE05C
    )
}

/// Keys that put a character into a text field: letters, digits,
/// punctuation and space.
pub fn is_printable(key: KeyCode) -> bool {
    matches!(key, 0x02..=0x0D | 0x10..=0x1B | 0x1E..=0x29 | 0x2B..=0x35 | 0x39 | 0x56)
}

/// Position of a key, or `None` for keys without a fixed place across
/// keyboards (media keys, the block above the arrows). Those still count for the rules that do
/// not need geometry.
pub fn position(key: KeyCode) -> Option<Pos> {
    let offset = |base: KeyCode| f32::from(key - base);
    match key {
        0x01 => at(0.0, 0.0),                          // Esc
        0x3B..=0x44 => at(0.0, 1.0 + offset(0x3B)),    // F1..F10
        0x57 => at(0.0, 11.0),                         // F11
        0x58 => at(0.0, 12.0),                         // F12

        0x29 => at(1.0, 0.0),                          // key left of 1
        0x02..=0x0D => at(1.0, 1.0 + offset(0x02)),    // 1 .. =
        0x0E => at(1.0, 13.5),                         // Backspace

        0x0F => at(2.0, 0.25),                         // Tab
        0x10..=0x1B => at(2.0, 1.5 + offset(0x10)),    // Q .. ]
        // ANSI puts this key at the end of the Q row, ISO at the end of the
        // home row. Halfway is adjacent to the right neighbours on both.
        0x2B => at(2.5, 13.25),

        0x3A => at(3.0, 0.4),                          // Caps Lock
        0x1E..=0x28 => at(3.0, 1.75 + offset(0x1E)),   // A .. '
        0x1C => at(3.0, 13.25),                        // Enter

        0x56 => at(4.0, 1.25),                         // ISO key left of Z
        0x2C..=0x35 => at(4.0, 2.25 + offset(0x2C)),   // Z .. /

        0x39 => Some(Pos { row: 5.0, x: 6.75, half_width: 2.75 }), // Space

        // The number pad, as on a full-size keyboard. Laptops squeeze it, but
        // which keys are neighbours stays the same. Without these a cat on
        // the number pad is invisible to every rule that needs geometry.
        0x45 => at(1.0, 17.0),                         // Num Lock
        0xE035 => at(1.0, 18.0),                       // /
        0x37 => at(1.0, 19.0),                         // *
        0x4A => at(1.0, 20.0),                         // -
        0x47..=0x49 => at(2.0, 17.0 + offset(0x47)),   // 7 8 9
        0x4E => at(2.5, 20.0),                         // +, two rows tall
        0x4B..=0x4D => at(3.0, 17.0 + offset(0x4B)),   // 4 5 6
        0x4F..=0x51 => at(4.0, 17.0 + offset(0x4F)),   // 1 2 3
        0xE01C => at(4.5, 20.0),                       // Enter, two rows tall
        0x52 => Some(Pos { row: 5.0, x: 17.5, half_width: 0.5 }), // 0, two keys wide
        0x53 => at(5.0, 19.0),                         // .

        0xE048 => at(4.0, 14.0),                       // Up
        0xE04B => at(5.0, 13.0),                       // Left
        0xE050 => at(5.0, 14.0),                       // Down
        0xE04D => at(5.0, 15.0),                       // Right
        _ => None,
    }
}

/// Height and width of the smallest box that touches every key. A wide key
/// is an interval, so it only adds width when the others lie beside it.
pub fn extent(keys: &[Pos]) -> (f32, f32) {
    let mut row_min = f32::MAX;
    let mut row_max = f32::MIN;
    let mut highest_left_edge = f32::MIN;
    let mut lowest_right_edge = f32::MAX;
    for p in keys {
        row_min = row_min.min(p.row);
        row_max = row_max.max(p.row);
        highest_left_edge = highest_left_edge.max(p.x - p.half_width);
        lowest_right_edge = lowest_right_edge.min(p.x + p.half_width);
    }
    (row_max - row_min, (highest_left_edge - lowest_right_edge).max(0.0))
}

/// Scancode of the key that prints `c` on a US layout. Test scenarios are
/// written as letters because `"wed"` reads better than `[0x11, 0x12, 0x20]`.
#[cfg(test)]
pub(crate) fn key_for(c: char) -> KeyCode {
    const ROWS: [(&str, KeyCode); 4] = [
        ("1234567890-=", 0x02),
        ("qwertyuiop[]", 0x10),
        ("asdfghjkl;'", 0x1E),
        ("zxcvbnm,./", 0x2C),
    ];
    if c == ' ' {
        return 0x39;
    }
    for (chars, base) in ROWS {
        if let Some(i) = chars.find(c) {
            return base + i as KeyCode;
        }
    }
    panic!("no key for {c:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(chars: &str) -> (f32, f32) {
        let keys: Vec<Pos> = chars.chars().map(|c| position(key_for(c)).unwrap()).collect();
        extent(&keys)
    }

    #[test]
    fn stagger_matches_a_real_keyboard() {
        assert_eq!(span("qa"), (1.0, 0.25));
        assert_eq!(span("az"), (1.0, 0.5));
        assert_eq!(span("wed"), (1.0, 1.25));
    }

    #[test]
    fn space_bar_is_next_to_every_key_above_it() {
        assert_eq!(span("v ").1, 0.0);
        assert_eq!(span("m ").1, 0.0);
        assert_eq!(span("z "), (1.0, 1.75));
    }

    #[test]
    fn the_number_pad_has_neighbours() {
        let pad = |codes: &[KeyCode]| extent(&codes.iter().map(|&c| position(c).unwrap()).collect::<Vec<_>>());
        assert_eq!(pad(&[0x4C, 0x4D, 0x48, 0x49]), (1.0, 1.0)); // 5 6 8 9, one paw
        assert_eq!(pad(&[0x4E, 0x4A]), (1.5, 0.0));             // + under -
        assert_eq!(pad(&[0x52, 0x4F]).1, 0.0);                  // 0 under 1
    }

    #[test]
    fn modifiers_have_no_say() {
        assert!(is_modifier(0x2A));
        assert!(is_modifier(0xE05B));
        assert!(!is_modifier(key_for('a')));
    }
}
