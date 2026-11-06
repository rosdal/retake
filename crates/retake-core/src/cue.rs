//! The short tones played when a clip hotkey is pressed and when the clip is written, built as
//! in-memory WAV files so no audio asset ships with Retake.

pub struct Tone {
    pub hz: u32,
    pub ms: u32,
}

pub const PRESS_CUE: Tone = Tone { hz: 880, ms: 70 };
pub const SAVED_CUE: Tone = Tone { hz: 1320, ms: 110 };

const RATE: u32 = 44_100;
const FADE_MS: u32 = 5;
const AMPLITUDE: f32 = 12_000.0;

/// A complete 16-bit mono 44.1 kHz WAV file of a sine at `t.hz` lasting `t.ms`, with 5 ms
/// fades so it doesn't click.
pub fn tone_wav(t: Tone) -> Vec<u8> {
    let n = RATE * t.ms / 1000;
    let fade = (RATE * FADE_MS / 1000).max(1);
    let data_len = n * 2;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes()); // byte rate
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..n {
        let env = (i.min(n - 1 - i) as f32 / fade as f32).min(1.0);
        let phase = 2.0 * std::f32::consts::PI * t.hz as f32 * i as f32 / RATE as f32;
        let v = (phase.sin() * AMPLITUDE * env) as i16;
        w.extend_from_slice(&v.to_le_bytes());
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
    }

    #[test]
    fn wav_header_and_length() {
        let w = tone_wav(Tone { hz: 1000, ms: 100 });
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..16], b"WAVEfmt ");
        assert_eq!(&w[36..40], b"data");
        let samples = 4410;
        assert_eq!(u32_at(&w, 40), samples * 2);
        assert_eq!(w.len(), 44 + samples as usize * 2);
        assert_eq!(u32_at(&w, 4), w.len() as u32 - 8);
        assert_eq!(u32_at(&w, 24), 44_100);
    }

    #[test]
    fn starts_and_ends_silent_and_is_not_clipped() {
        let w = tone_wav(PRESS_CUE);
        let s: Vec<i16> = w[44..]
            .chunks(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        assert_eq!(s[0], 0);
        assert!(s.last().unwrap().abs() < 300);
        let peak = s.iter().map(|v| v.unsigned_abs()).max().unwrap();
        assert!((8_000..=16_000).contains(&peak), "{peak}");
    }

    #[test]
    fn the_two_cues_differ() {
        assert_ne!(tone_wav(PRESS_CUE), tone_wav(SAVED_CUE));
    }
}
