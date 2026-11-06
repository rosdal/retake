//! Audio logic that needs no Windows: placing captured packets on a continuous timeline
//! (silence filling), the mic noise gate and sample conversion. Everything is 48 kHz,
//! interleaved stereo i16 once it leaves [`to_stereo_i16`].

/// Sample rate of every pipe into ffmpeg.
pub const RATE: u32 = 48_000;
/// Channels on every pipe.
pub const CHANNELS: usize = 2;
/// Frames per 10 ms pump tick.
pub const TICK_FRAMES: usize = 480;

/// Stream position (frames since the run's origin) of a time `us` microseconds after it.
pub fn frames_at(us: i64) -> i64 {
    us * i64::from(RATE) / 1_000_000
}

/// Packets placed further ahead of the emitted position than this (10 s) are ignored.
const MAX_AHEAD: usize = 10 * RATE as usize;

/// A continuous audio stream built from packets that arrive late, early, overlapping or not at
/// all. Each packet is placed at the position it was captured; [`Timeline::take_due`] hands
/// out every frame up to "now minus the hold", with zeros wherever nothing arrived. The output
/// therefore has exactly [`RATE`] frames per second of wall time, and a sample's place in it
/// is its capture time.
pub struct Timeline {
    hold: u64,
    /// Next position to emit.
    emitted: u64,
    /// Frames placed but not emitted yet, starting at `emitted`. `None` = nothing arrived.
    pending: Vec<Option<[i16; 2]>>,
    late: u64,
}

impl Timeline {
    /// `hold` frames are kept back from "now" so packets that arrive up to that late still
    /// land where they belong.
    pub fn new(hold: u64) -> Timeline {
        Timeline {
            hold,
            emitted: 0,
            pending: Vec::new(),
            late: 0,
        }
    }

    /// Places interleaved stereo `frames` starting at stream position `pos`. Frames before the
    /// already-emitted position are dropped and counted; where two packets overlap, the first
    /// one wins.
    pub fn push(&mut self, pos: i64, frames: &[i16]) {
        let n = frames.len() / CHANNELS;
        let emitted = self.emitted as i64;
        let skip = (emitted - pos).clamp(0, n as i64) as usize;
        self.late += skip as u64;
        let first = (pos + skip as i64 - emitted) as usize;
        if first > MAX_AHEAD {
            // A capture time this far ahead is a broken clock, not audio to keep.
            return;
        }
        let end = first + (n - skip);
        if self.pending.len() < end {
            self.pending.resize(end, None);
        }
        for (i, f) in frames
            .as_chunks::<CHANNELS>()
            .0
            .iter()
            .enumerate()
            .skip(skip)
        {
            let slot = &mut self.pending[first + i - skip];
            if slot.is_none() {
                *slot = Some([f[0], f[1]]);
            }
        }
    }

    /// Every frame from the last emitted position up to `now_pos - hold`, interleaved stereo.
    pub fn take_due(&mut self, now_pos: u64) -> Vec<i16> {
        let target = now_pos.saturating_sub(self.hold);
        if target <= self.emitted {
            return Vec::new();
        }
        let n = (target - self.emitted) as usize;
        let mut out = Vec::with_capacity(n * CHANNELS);
        let have = n.min(self.pending.len());
        for f in self.pending.drain(..have) {
            out.extend_from_slice(&f.unwrap_or([0, 0]));
        }
        out.resize(n * CHANNELS, 0);
        self.emitted = target;
        out
    }

    /// Frames dropped because they arrived after their position was emitted.
    pub fn late_frames(&self) -> u64 {
        self.late
    }
}

/// A simple noise gate: RMS over 10 ms windows, open above the threshold, 5 ms attack and
/// 150 ms release, so breathing and keyboard noise drop out.
pub struct Gate {
    /// Linear RMS threshold, or `None` when the gate is off.
    threshold: Option<f32>,
    gain: f32,
}

/// Gain change per frame while opening (5 ms) and closing (150 ms).
const ATTACK_STEP: f32 = 1.0 / 240.0;
const RELEASE_STEP: f32 = 1.0 / 7_200.0;

impl Gate {
    /// `threshold_db` in dBFS; `0.0` turns the gate off.
    pub fn new(threshold_db: f32) -> Gate {
        Gate {
            threshold: (threshold_db < 0.0).then(|| 10f32.powf(threshold_db / 20.0) * 32_768.0),
            gain: 0.0,
        }
    }

    /// Gates interleaved stereo frames in place, deciding per 10 ms window.
    pub fn process(&mut self, frames: &mut [i16]) {
        let Some(threshold) = self.threshold else {
            return;
        };
        for window in frames.chunks_mut(TICK_FRAMES * CHANNELS) {
            let sum: f64 = window.iter().map(|&s| f64::from(s).powi(2)).sum();
            let rms = (sum / window.len() as f64).sqrt() as f32;
            let open = rms > threshold;
            for f in window.chunks_mut(CHANNELS) {
                self.gain = if open {
                    (self.gain + ATTACK_STEP).min(1.0)
                } else {
                    (self.gain - RELEASE_STEP).max(0.0)
                };
                for s in f {
                    *s = (f32::from(*s) * self.gain) as i16;
                }
            }
        }
    }
}

/// Raw samples as WASAPI delivers them.
pub enum SampleSource<'a> {
    F32 { data: &'a [f32], channels: usize },
    I16 { data: &'a [i16], channels: usize },
}

/// Converts to interleaved stereo i16: mono is duplicated, more than two channels keep the
/// first two.
pub fn to_stereo_i16(src: SampleSource<'_>) -> Vec<i16> {
    fn pick<T: Copy>(data: &[T], channels: usize, conv: impl Fn(T) -> i16) -> Vec<i16> {
        let channels = channels.max(1);
        let mut out = Vec::with_capacity(data.len() / channels * CHANNELS);
        for f in data.chunks_exact(channels) {
            let l = conv(f[0]);
            let r = if channels > 1 { conv(f[1]) } else { l };
            out.extend_from_slice(&[l, r]);
        }
        out
    }
    match src {
        SampleSource::F32 { data, channels } => {
            pick(data, channels, |v| (v.clamp(-1.0, 1.0) * 32_767.0) as i16)
        }
        SampleSource::I16 { data, channels } => pick(data, channels, |v| v),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo(n: usize, v: i16) -> Vec<i16> {
        vec![v; n * 2]
    }

    #[test]
    fn exactly_48000_frames_per_second_with_gaps() {
        let mut t = Timeline::new(0);
        let mut out = 0usize;
        // Packets of 10 ms for one simulated second, every third one missing.
        for tick in 0..100u64 {
            if tick % 3 != 0 {
                t.push((tick * 480) as i64, &stereo(480, 7));
            }
            out += t.take_due((tick + 1) * 480).len() / 2;
        }
        assert_eq!(out, 48_000);
    }

    #[test]
    fn samples_land_at_their_position_with_zeros_around() {
        let mut t = Timeline::new(0);
        t.push(100, &stereo(10, 5));
        let out = t.take_due(200);
        assert_eq!(out.len(), 400);
        assert!(out[..200].iter().all(|&s| s == 0));
        assert!(out[200..220].iter().all(|&s| s == 5));
        assert!(out[220..].iter().all(|&s| s == 0));
    }

    #[test]
    fn hold_keeps_room_for_late_packets() {
        let mut t = Timeline::new(240);
        assert_eq!(t.take_due(480).len(), 240 * 2);
        // Arrives 10 ms after it was captured: still in time.
        t.push(300, &stereo(100, 9));
        let out = t.take_due(960);
        assert_eq!(out.len(), 480 * 2);
        assert!(
            out[(300 - 240) * 2..(400 - 240) * 2]
                .iter()
                .all(|&s| s == 9)
        );
        assert_eq!(t.late_frames(), 0);
    }

    #[test]
    fn too_late_frames_are_trimmed_and_counted() {
        let mut t = Timeline::new(0);
        t.take_due(1000);
        t.push(990, &stereo(20, 3));
        assert_eq!(t.late_frames(), 10);
        let out = t.take_due(1020);
        assert!(out[..20].iter().all(|&s| s == 3));
        assert!(out[20..].iter().all(|&s| s == 0));
    }

    #[test]
    fn overlap_keeps_the_first_write() {
        let mut t = Timeline::new(0);
        t.push(0, &stereo(10, 1));
        t.push(5, &stereo(10, 2));
        let out = t.take_due(15);
        assert!(out[..20].iter().all(|&s| s == 1));
        assert!(out[20..30].iter().all(|&s| s == 2));
    }

    #[test]
    fn negative_positions_are_late() {
        let mut t = Timeline::new(0);
        t.push(-5, &stereo(10, 4));
        assert_eq!(t.late_frames(), 5);
        assert!(t.take_due(5).iter().all(|&s| s == 4));
    }

    #[test]
    fn frames_at_converts_microseconds() {
        assert_eq!(frames_at(1_000_000), 48_000);
        assert_eq!(frames_at(10_000), 480);
        assert_eq!(frames_at(-1_000), -48);
    }

    fn sine(frames: usize, amp: f32) -> Vec<i16> {
        (0..frames)
            .flat_map(|i| {
                let v =
                    (amp * (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0).sin()) as i16;
                [v, v]
            })
            .collect()
    }

    fn energy(s: &[i16]) -> f64 {
        s.iter().map(|&v| f64::from(v).powi(2)).sum::<f64>() / s.len().max(1) as f64
    }

    #[test]
    fn gate_passes_loud_and_silences_a_quiet_tail() {
        let mut g = Gate::new(-40.0);
        let mut loud = sine(48_000, 16_000.0); // about -6 dBFS
        let mut quiet = sine(48_000, 100.0); // about -50 dBFS
        g.process(&mut loud);
        g.process(&mut quiet);
        let orig = sine(48_000, 16_000.0);
        // The loud part passes, apart from the attack at the very start.
        assert!(energy(&loud[4_800..]) > 0.95 * energy(&orig[4_800..]));
        // After the 150 ms release the quiet tail is gone.
        assert!(quiet[2 * 9_600..].iter().all(|&s| s == 0));
    }

    #[test]
    fn gate_off_changes_nothing() {
        let mut g = Gate::new(0.0);
        let mut s = sine(4_800, 50.0);
        let orig = s.clone();
        g.process(&mut s);
        assert_eq!(s, orig);
    }

    #[test]
    fn converts_float_mono_and_surround() {
        assert_eq!(
            to_stereo_i16(SampleSource::F32 {
                data: &[0.5, -1.0, 2.0],
                channels: 1
            }),
            [16_383, 16_383, -32_767, -32_767, 32_767, 32_767]
        );
        assert_eq!(
            to_stereo_i16(SampleSource::I16 {
                data: &[1, 2, 3, 4, 5, 6],
                channels: 3
            }),
            [1, 2, 4, 5]
        );
        assert_eq!(
            to_stereo_i16(SampleSource::I16 {
                data: &[1, 2, 3, 4],
                channels: 2
            }),
            [1, 2, 3, 4]
        );
    }
}
