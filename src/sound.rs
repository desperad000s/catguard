//! The deterrent: a harmonica blown hard, synthesized so that the project
//! ships no recording.
//!
//! A harmonica reed is close to a sawtooth with a nasal bump around the
//! second to fourth harmonic. Two reeds tuned a few hertz apart give the
//! wavering tone of a tremolo harmonica. The phrase is a blow chord followed
//! by a draw chord a step up, which is the wheeze everybody recognizes.

use std::f32::consts::TAU;

pub const SAMPLE_RATE: u32 = 22_050;

const BLOW: [f32; 4] = [261.63, 329.63, 392.00, 523.25]; // C4 E4 G4 C5
const DRAW: [f32; 4] = [293.66, 392.00, 493.88, 587.33]; // D4 G4 B4 D5
const CHORD_SECONDS: f32 = 0.55;
const DETUNE_HZ: f32 = 5.0;
const HARMONICS: [f32; 8] = [1.0, 0.9, 0.8, 0.55, 0.3, 0.2, 0.12, 0.08];

fn reed(freq: f32, t: f32) -> f32 {
    HARMONICS
        .iter()
        .enumerate()
        .map(|(i, amp)| amp * (TAU * freq * (i + 1) as f32 * t).sin())
        .sum()
}

fn chord(notes: &[f32; 4], out: &mut Vec<f32>) {
    let count = (CHORD_SECONDS * SAMPLE_RATE as f32) as usize;
    let mut noise_state: u32 = 0x2545_F491;
    for n in 0..count {
        let t = n as f32 / SAMPLE_RATE as f32;
        let attack = (t / 0.03).min(1.0);
        let release = ((CHORD_SECONDS - t) / 0.06).min(1.0);
        let mut sample = 0.0;
        for &freq in notes {
            sample += reed(freq, t) + reed(freq + DETUNE_HZ, t);
        }
        // Breath.
        noise_state = noise_state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let noise = (noise_state >> 16) as f32 / 32_768.0 - 1.0;
        out.push((sample + 1.5 * noise) * attack * release);
    }
}

/// The whole phrase as a mono 16-bit PCM WAV file.
pub fn harmonica_wav() -> Vec<u8> {
    let mut samples = Vec::new();
    chord(&BLOW, &mut samples);
    chord(&DRAW, &mut samples);

    let peak = samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
    let scale = 0.9 * f32::from(i16::MAX) / peak;

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

    #[test]
    fn wav_is_well_formed_and_loud() {
        let wav = harmonica_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..16], b"WAVEfmt ");
        let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize;
        let data_len = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert_eq!(riff_len, wav.len() - 8);
        assert_eq!(data_len, wav.len() - 44);

        let samples: Vec<i16> = wav[44..]
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(samples.len(), 2 * (CHORD_SECONDS * SAMPLE_RATE as f32) as usize);
        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 29_000, "peak {peak}");
        let rms = (samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>()
            / samples.len() as f64)
            .sqrt();
        assert!(rms > 3_000.0, "rms {rms}");
    }
}
