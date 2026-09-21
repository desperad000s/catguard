//! What the user can change, kept as a small JSON file.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::detector::Sensitivity;
use crate::guard::UNLOCK_WORD;
use crate::sound::Sound;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub sensitivity: Sensitivity,
    pub sound: bool,
    pub sound_kind: Sound,
    pub word: String,
    /// "dark", "light" or "system".
    pub theme: String,
    /// How often the keyboard was locked. The only thing catguard remembers
    /// about the cat across restarts.
    pub locks: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sensitivity: Sensitivity::Normal, sound: true, sound_kind: Sound::Harmonica, word: UNLOCK_WORD.into(), theme: "dark".into(), locks: 0 }
    }
}

impl Settings {
    /// A missing or broken file gives the defaults. Settings are never a
    /// reason not to guard the keyboard.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Settings>(&text).ok())
            .unwrap_or_default()
            .sanitized()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).expect("settings serialize"))
    }

    /// The unlock word is matched against virtual-key codes, which only
    /// works for plain letters. Anything else would lock the user out.
    pub fn sanitized(mut self) -> Self {
        self.word = self.word.to_ascii_lowercase();
        let usable = (3..=12).contains(&self.word.len()) && self.word.bytes().all(|b| b.is_ascii_lowercase());
        if !usable {
            self.word = UNLOCK_WORD.into();
        }
        if !matches!(self.theme.as_str(), "dark" | "light" | "system") {
            self.theme = "dark".into();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_that_cannot_be_typed_blind_falls_back_to_human() {
        for bad in ["", "ab", "pass word", "gr\u{fc}n", "h4ck", "averyveryverylongword"] {
            let settings = Settings { word: bad.into(), ..Settings::default() }.sanitized();
            assert_eq!(settings.word, "human", "{bad:?}");
        }
        assert_eq!(Settings { word: "Mensch".into(), ..Settings::default() }.sanitized().word, "mensch");
    }

    #[test]
    fn survives_a_round_trip_and_a_broken_file() {
        let dir = std::env::temp_dir().join(format!("catguard-test-{}", std::process::id()));
        let path = dir.join("settings.json");
        let settings = Settings { sensitivity: Sensitivity::Kitten, sound: false, locks: 7, ..Settings::default() };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);

        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::write(&path, r#"{"sound": false}"#).unwrap();
        assert!(!Settings::load(&path).sound);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
