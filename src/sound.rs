//! The deterrents, synthesized so that the project ships no recordings.
//!
//! None of these is proven to work on every cat. They are the sounds that
//! deterrents commonly use: PawSense plays a harmonica, a hiss is the cat's
//! own warning, compressed-air cans hiss at the cat, and ultrasonic devices
//! use tones at the top of human hearing.

use std::f32::consts::TAU;

use serde::{Deserialize, Serialize};

pub const SAMPLE_RATE: u32 = 44_100;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sound {
    #[default]
    Harmonica,
    Hiss,
    Spray,
    Whistle,
}

impl Sound {
    pub const ALL: [Sound; 4] = [Sound::Harmonica, Sound::Hiss, Sound::Spray, Sound::Whistle];

    /// The sound as a mono 16-bit PCM WAV file.
    pub fn wav(self) -> Vec<u8> {
        let samples = match self {
            Sound::Harmonica => harmonica(),
            Sound::Hiss => hiss(),
            Sound::Spray => spray(),
            Sound::Whistle => whistle(),
        };
        // The whistle sits where ears are most easily annoyed and speakers
        // distort first, so it stays below full scale.
        encode(&samples, if self == Sound::Whistle { 0.5 } else { 0.9 })
    }
}

fn seconds(duration: f32) -> impl Iterator<Item = f32> {
    (0..(duration * SAMPLE_RATE as f32) as usize).map(|n| n as f32 / SAMPLE_RATE as f32)
}

/// White noise in -1..1 from a linear congruential generator. Deterministic,
/// so the tests see the same sound the user hears.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 >> 16) as f32 / 32_768.0 - 1.0
    }
}

/// A harmonica reed is close to a sawtooth with a nasal bump around the
/// second to fourth harmonic. Two reeds tuned a few hertz apart give the
/// wavering tone of a tremolo harmonica. The phrase is a blow chord followed
/// by a draw chord a step up, the wheeze everybody recognizes.
fn harmonica() -> Vec<f32> {
    const BLOW: [f32; 4] = [261.63, 329.63, 392.00, 523.25]; // C4 E4 G4 C5
    const DRAW: [f32; 4] = [293.66, 392.00, 493.88, 587.33]; // D4 G4 B4 D5
    const CHORD: f32 = 0.55;
    const HARMONICS: [f32; 8] = [1.0, 0.9, 0.8, 0.55, 0.3, 0.2, 0.12, 0.08];
    let reed = |freq: f32, t: f32| -> f32 {
        HARMONICS.iter().enumerate().map(|(i, amp)| amp * (TAU * freq * (i + 1) as f32 * t).sin()).sum()
    };
    let mut breath = Noise(0x2545_F491);
    let mut out = Vec::new();
    for notes in [BLOW, DRAW] {
        for t in seconds(CHORD) {
            let envelope = (t / 0.03).min(1.0) * ((CHORD - t) / 0.06).min(1.0);
            let chord: f32 = notes.iter().map(|&f| reed(f, t) + reed(f + 5.0, t)).sum();
            out.push((chord + 1.5 * breath.next()) * envelope);
        }
    }
    out
}

/// Noise with the lows taken out. `brightness` near 1 leaves only the top.
fn hissing(noise: &mut Noise, previous: &mut f32, brightness: f32) -> f32 {
    let white = noise.next();
    let high = white - brightness * *previous;
    *previous = white;
    high
}

/// A cat's hiss: a short spit, then breath that flutters and fades.
fn hiss() -> Vec<f32> {
    const LENGTH: f32 = 0.95;
    let (mut noise, mut previous) = (Noise(0x9E37_79B9), 0.0);
    seconds(LENGTH)
        .map(|t| {
            let spit = (-t / 0.035).exp();
            let breath = (t / 0.02).min(1.0) * (1.0 - t / LENGTH).powf(0.7);
            let flutter = 1.0 + 0.25 * (TAU * 27.0 * t).sin();
            hissing(&mut noise, &mut previous, 0.82) * (1.4 * spit + breath * flutter)
        })
        .collect()
}

/// Two bursts from a can of compressed air: hard onset, bright, quickly gone.
fn spray() -> Vec<f32> {
    let (mut noise, mut previous) = (Noise(0x1234_5678), 0.0);
    seconds(0.62)
        .map(|t| {
            let burst = |start: f32| if t < start { 0.0 } else { ((t - start) / 0.003).min(1.0) * (-(t - start) / 0.07).exp() };
            hissing(&mut noise, &mut previous, 0.96) * (burst(0.0) + burst(0.3))
        })
        .collect()
}

/// A tone that jumps between 15 and 17.5 kHz. Most adults barely hear it,
/// cats hear it well. Laptop speakers reach this high, but only just.
fn whistle() -> Vec<f32> {
    const LENGTH: f32 = 0.8;
    let mut phase = 0.0_f32;
    seconds(LENGTH)
        .map(|t| {
            let freq = if ((t / 0.1) as u32).is_multiple_of(2) { 15_000.0 } else { 17_500.0 };
            phase = (phase + TAU * freq / SAMPLE_RATE as f32) % TAU;
            phase.sin() * (t / 0.01).min(1.0) * ((LENGTH - t) / 0.02).min(1.0)
        })
        .collect()
}

fn encode(samples: &[f32], level: f32) -> Vec<u8> {
    let peak = samples.iter().fold(f32::MIN_POSITIVE, |m, s| m.max(s.abs()));
    let scale = level * f32::from(i16::MAX) / peak;

    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16_u32.to_le_bytes());
    wav.extend_from_slice(&1_u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1_u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // bytes per second
    wav.extend_from_slice(&2_u16.to_le_bytes()); // bytes per frame
    wav.extend_from_slice(&16_u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&((s * scale) as i16).to_le_bytes());
    }
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(wav: &[u8]) -> Vec<i16> {
        wav[44..].chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect()
    }

    /// Share of sign changes per sample: near 0 for low tones, 0.5 for white
    /// noise, towards 1 for tones at the top of the spectrum.
    fn crossings(samples: &[i16]) -> f32 {
        let changes = samples.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
        changes as f32 / samples.len() as f32
    }

    #[test]
    fn every_sound_is_a_well_formed_wav_that_is_not_silent() {
        for sound in Sound::ALL {
            let wav = sound.wav();
            assert_eq!(&wav[0..4], b"RIFF", "{sound:?}");
            assert_eq!(&wav[8..16], b"WAVEfmt ");
            assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize, wav.len() - 8);
            assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize, wav.len() - 44);

            let samples = pcm(&wav);
            let length = samples.len() as f32 / SAMPLE_RATE as f32;
            assert!((0.5..1.5).contains(&length), "{sound:?} lasts {length} s");
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
            assert!(peak > 16_000, "{sound:?} peaks at {peak}");
        }
    }

    #[test]
    fn the_sounds_sit_where_they_should_in_the_spectrum() {
        let at = |sound: Sound| crossings(&pcm(&sound.wav()));
        assert!(at(Sound::Harmonica) < 0.15, "a chord around 300 to 600 Hz");
        assert!(at(Sound::Hiss) > 0.5, "noise without its lows");
        assert!(at(Sound::Spray) > at(Sound::Hiss), "brighter than the hiss");
        assert!((0.65..0.85).contains(&at(Sound::Whistle)), "15 to 17.5 kHz at 44.1 kHz");
    }

    #[test]
    fn the_spray_is_two_bursts_with_quiet_between() {
        let samples = pcm(&Sound::Spray.wav());
        let loudness = |from: f32, to: f32| {
            let range = (from * SAMPLE_RATE as f32) as usize..(to * SAMPLE_RATE as f32) as usize;
            samples[range].iter().map(|s| u32::from(s.unsigned_abs())).max().unwrap()
        };
        assert!(loudness(0.0, 0.05) > 4 * loudness(0.25, 0.3));
        assert!(loudness(0.3, 0.35) > 4 * loudness(0.25, 0.3));
    }
}
