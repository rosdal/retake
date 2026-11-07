//! Song analysis for montages, in plain Rust: beats (spectral-flux onsets, tempo by
//! autocorrelation, beat tracking by dynamic programming), bars (4/4, beat 1 = the phase with
//! the most kick energy), and the drop (the bar with the biggest energy jump over the bars
//! before it).

use serde::{Deserialize, Serialize};

/// Analysis sample rate: songs are decoded to mono at this rate.
pub const RATE: u32 = 22_050;
const FRAME: usize = 1024;
const HOP: usize = 512;

/// What a song looks like to the arranger. Times in seconds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Analysis {
    pub bpm: f64,
    pub beats: Vec<f64>,
    /// Index into `beats` of each bar's first beat.
    pub bars: Vec<usize>,
    /// Index into `bars` of the drop.
    pub drop_bar: usize,
    pub duration: f64,
}

impl Analysis {
    /// Time of the drop (its bar's first beat).
    pub fn drop_time(&self) -> f64 {
        self.bars
            .get(self.drop_bar)
            .and_then(|&b| self.beats.get(b))
            .copied()
            .unwrap_or(0.0)
    }

    /// Where a `length`-second montage should start in the song so the drop lands at about
    /// 35% of it, snapped to a bar start.
    pub fn window_start(&self, length: f64) -> f64 {
        let want = (self.drop_time() - 0.35 * length).max(0.0);
        self.bars
            .iter()
            .filter_map(|&b| self.beats.get(b).copied())
            .rfind(|&t| t <= want + 1e-9)
            .unwrap_or(0.0)
    }
}

/// In-place radix-2 FFT of `re`/`im` (length a power of two).
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * std::f32::consts::PI / len as f32;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f32, 0.0f32);
            for k in 0..len / 2 {
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
}

/// Per frame: (onset strength, low-band energy, RMS).
fn frames(samples: &[f32]) -> Vec<(f32, f32, f32)> {
    let window: Vec<f32> = (0..FRAME)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / FRAME as f32).cos())
        .collect();
    let low_bin = (150.0 * FRAME as f32 / RATE as f32).ceil() as usize;
    // Onsets from below 3 kHz: kick and snare carry the beat; hi-hats (above) would pull the
    // tracker onto the off-beats.
    let flux_bins = (3_000.0 * FRAME as f32 / RATE as f32) as usize;
    let mut prev = vec![0.0f32; FRAME / 2];
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + FRAME <= samples.len() {
        let chunk = &samples[pos..pos + FRAME];
        let mut re: Vec<f32> = chunk.iter().zip(&window).map(|(s, w)| s * w).collect();
        let mut im = vec![0.0f32; FRAME];
        fft(&mut re, &mut im);
        let mut flux = 0.0f32;
        let mut low = 0.0f32;
        for k in 1..FRAME / 2 {
            let mag = (re[k] * re[k] + im[k] * im[k]).sqrt();
            let lm = (1.0 + 100.0 * mag).ln();
            let d = lm - prev[k];
            if d > 0.0 && k <= flux_bins {
                // The kick band counts more: it carries the beat.
                flux += if k <= low_bin * 2 { 4.0 * d } else { d };
            }
            prev[k] = lm;
            if k <= low_bin {
                low += mag;
            }
        }
        let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        out.push((flux, low, rms));
        pos += HOP;
    }
    out
}

/// A frame's time: the middle of its window, where an onset it reacts to sits on average.
fn frame_time(i: usize) -> f64 {
    (i * HOP + FRAME / 2) as f64 / f64::from(RATE)
}

/// Beats per minute from the onset envelope's autocorrelation, 70–180 BPM, favouring
/// 90–150 a little (octave errors are the usual failure).
fn tempo(onset: &[f32]) -> f64 {
    let fps = f64::from(RATE) / HOP as f64;
    let (min_lag, max_lag) = ((fps * 60.0 / 180.0) as usize, (fps * 60.0 / 70.0) as usize);
    // Smooth over ~3 frames so a beat period that falls between two whole frames still lines
    // its peaks up.
    let smooth: Vec<f32> = (0..onset.len())
        .map(|i| {
            let at = |j: isize| onset.get(i.wrapping_add_signed(j)).copied().unwrap_or(0.0);
            0.25 * at(-1) + 0.5 * at(0) + 0.25 * at(1)
        })
        .collect();
    let mean = smooth.iter().sum::<f32>() / smooth.len().max(1) as f32;
    let o: Vec<f32> = smooth.iter().map(|v| v - mean).collect();
    let ac = |lag: usize| -> f64 {
        if lag >= o.len() {
            return 0.0;
        }
        o.iter().zip(&o[lag..]).map(|(a, b)| f64::from(a * b)).sum()
    };
    let mut best = (0.0f64, 120.0f64);
    for lag in min_lag..=max_lag.min(o.len().saturating_sub(1)) {
        // A comb over the lag and its multiples: the true beat period repeats at 2 and 4
        // beats, while an off-beat pattern's lag (e.g. three eighth notes) doesn't.
        let r = ac(lag) + 0.5 * ac(2 * lag) + 0.25 * ac(4 * lag);
        let bpm = 60.0 * fps / lag as f64;
        let prior = (-((bpm / 120.0).log2()).powi(2) / (2.0 * 0.6f64.powi(2))).exp();
        let score = r * (0.6 + 0.4 * prior);
        if score > best.0 {
            best = (score, bpm);
        }
    }
    best.1
}

/// Beat frames by dynamic programming (Ellis 2007): each beat rewards onset strength and
/// penalises gaps that differ from the tempo's period.
fn track(onset: &[f32], bpm: f64) -> Vec<usize> {
    let fps = f64::from(RATE) / HOP as f64;
    let period = fps * 60.0 / bpm;
    let n = onset.len();
    if n == 0 {
        return Vec::new();
    }
    let max = onset.iter().cloned().fold(0.0f32, f32::max).max(1e-6);
    let o: Vec<f64> = onset.iter().map(|v| f64::from(*v / max)).collect();
    let mut score = o.clone();
    let mut back = vec![usize::MAX; n];
    // How strongly gaps that differ from the period are penalised (onsets are 0–1).
    let tightness = 50.0;
    for t in 0..n {
        let lo = (t as f64 - 2.0 * period).max(0.0) as usize;
        let hi = (t as f64 - 0.5 * period).max(0.0) as usize;
        let mut best = (f64::MIN, usize::MAX);
        for (p, &sp) in score.iter().enumerate().take(hi.min(t)).skip(lo) {
            let gap = (t - p) as f64;
            let s = sp - tightness * (gap / period).ln().powi(2);
            if s > best.0 {
                best = (s, p);
            }
        }
        if best.1 != usize::MAX {
            score[t] = o[t] + best.0;
            back[t] = best.1;
        }
    }
    // Start from the best-scoring frame in the last period.
    let tail = (n as f64 - period).max(0.0) as usize;
    let mut t = (tail..n)
        .max_by(|&a, &b| score[a].total_cmp(&score[b]))
        .unwrap_or(n - 1);
    let mut beats = vec![t];
    while back[t] != usize::MAX {
        t = back[t];
        beats.push(t);
    }
    beats.reverse();
    beats
}

/// Analyses mono samples at [`RATE`].
pub fn analyse(samples: &[f32]) -> Option<Analysis> {
    let f = frames(samples);
    if f.len() < 64 {
        return None;
    }
    let onset: Vec<f32> = f.iter().map(|x| x.0).collect();
    let mut bpm = tempo(&onset);
    let beat_frames = track(&onset, bpm);
    if beat_frames.len() < 8 {
        return None;
    }
    let beats: Vec<f64> = beat_frames.iter().map(|&i| frame_time(i)).collect();
    // Refine the BPM from the tracked beats' median gap.
    let mut gaps: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    gaps.sort_by(f64::total_cmp);
    if let Some(g) = gaps.get(gaps.len() / 2)
        && *g > 0.0
    {
        bpm = 60.0 / g;
    }
    // Downbeat phase: the beat phase (mod 4) with the most low-band energy.
    let low_at = |i: usize| -> f32 {
        let fr = beat_frames[i];
        (fr.saturating_sub(1)..(fr + 2).min(f.len()))
            .map(|k| f[k].1)
            .sum()
    };
    let phase = (0..4)
        .max_by(|&a, &b| {
            let s = |p: usize| (p..beats.len()).step_by(4).map(low_at).sum::<f32>();
            s(a).total_cmp(&s(b))
        })
        .unwrap_or(0);
    let bars: Vec<usize> = (phase..beats.len()).step_by(4).collect();
    // RMS per bar, the drop = the biggest jump above every one of the 4 bars before it.
    let rms_between = |a: f64, b: f64| -> f32 {
        let (fa, fb) = (
            (a * f64::from(RATE) / HOP as f64) as usize,
            (b * f64::from(RATE) / HOP as f64) as usize,
        );
        let s = &f[fa.min(f.len())..fb.min(f.len())];
        s.iter().map(|x| x.2).sum::<f32>() / s.len().max(1) as f32
    };
    let energy: Vec<f32> = bars
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            let end = bars
                .get(i + 1)
                .map_or(*beats.last().unwrap_or(&0.0), |&n| beats[n]);
            rms_between(beats[b], end)
        })
        .collect();
    let drop_bar = (4..energy.len())
        .max_by(|&a, &b| {
            let jump = |i: usize| energy[i] - energy[i - 4..i].iter().cloned().fold(0.0, f32::max);
            jump(a).total_cmp(&jump(b))
        })
        .unwrap_or(0);
    Some(Analysis {
        bpm,
        beats,
        bars,
        drop_bar,
        duration: samples.len() as f64 / f64::from(RATE),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic song: a kick on every beat (louder on beat 1), hats in between, and the
    /// whole mix getting much louder from bar `drop_bar` on.
    fn song(bpm: f64, bars: usize, drop_bar: usize) -> Vec<f32> {
        let beat = 60.0 / bpm;
        let n = (bars as f64 * 4.0 * beat * f64::from(RATE)) as usize;
        let mut s = vec![0.0f32; n];
        for b in 0..bars * 4 {
            let t0 = (b as f64 * beat * f64::from(RATE)) as usize;
            let loud = if b / 4 >= drop_bar { 1.0 } else { 0.25 };
            let accent = if b % 4 == 0 { 1.0 } else { 0.6 };
            // Kick: a pitch-dropping low sine, ~150 ms.
            for i in 0..6_000.min(n.saturating_sub(t0)) {
                let tt = i as f32 / RATE as f32;
                let f = 50.0 + 100.0 * (-tt / 0.03).exp();
                s[t0 + i] += loud
                    * accent
                    * (-tt / 0.15).exp()
                    * (2.0 * std::f32::consts::PI * f * tt).sin();
            }
            // Hat on the off-beat: a short 7 kHz burst.
            let h0 = t0 + (beat * 0.5 * f64::from(RATE)) as usize;
            for i in 0..800.min(n.saturating_sub(h0)) {
                let tt = i as f32 / RATE as f32;
                s[h0 + i] += loud
                    * 0.3
                    * (-tt / 0.02).exp()
                    * (2.0 * std::f32::consts::PI * 7_000.0 * tt).sin();
            }
        }
        s
    }

    #[test]
    fn fft_finds_a_sine() {
        let mut re: Vec<f32> = (0..64)
            .map(|i| (2.0 * std::f32::consts::PI * 5.0 * i as f32 / 64.0).sin())
            .collect();
        let mut im = vec![0.0; 64];
        fft(&mut re, &mut im);
        let mags: Vec<f32> = re
            .iter()
            .zip(&im)
            .map(|(r, i)| (r * r + i * i).sqrt())
            .collect();
        let peak = mags[..32]
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(peak, 5);
    }

    #[test]
    fn known_tempos_are_found() {
        for bpm in [90.0, 110.0, 128.0, 140.0, 174.0] {
            let a = analyse(&song(bpm, 24, 16)).unwrap();
            // Octave errors (half/double) count as wrong.
            assert!(
                (a.bpm - bpm).abs() / bpm < 0.03,
                "{bpm} BPM detected as {:.1}",
                a.bpm
            );
        }
    }

    #[test]
    fn drop_is_within_a_bar() {
        let bpm = 128.0;
        let a = analyse(&song(bpm, 24, 16)).unwrap();
        let bar = 4.0 * 60.0 / bpm;
        let want = 16.0 * bar;
        assert!(
            (a.drop_time() - want).abs() <= bar + 1e-6,
            "drop at {:.2}, want {want:.2}",
            a.drop_time()
        );
    }

    #[test]
    fn beats_are_on_time() {
        let bpm = 120.0;
        let a = analyse(&song(bpm, 16, 12)).unwrap();
        let err: Vec<f64> = a
            .beats
            .iter()
            .map(|t| {
                let k = (t / 0.5).round();
                t - k * 0.5
            })
            .collect();
        let mean = err.iter().sum::<f64>() / err.len() as f64;
        let worst = err.iter().fold(0.0f64, |m, e| m.max(e.abs()));
        assert!(worst < 0.03, "beat error mean {mean:.4} worst {worst:.4}");
    }

    #[test]
    fn downbeat_is_the_accented_beat() {
        let bpm = 120.0;
        let a = analyse(&song(bpm, 16, 12)).unwrap();
        let beat = 0.5;
        // Every bar start sits on a multiple of 4 beats.
        for &b in &a.bars[1..] {
            let k = a.beats[b] / beat;
            assert!(
                (k / 4.0 - (k / 4.0).round()).abs() < 0.1,
                "bar start at beat {k:.2}"
            );
        }
    }

    #[test]
    fn window_puts_the_drop_near_a_third() {
        let a = analyse(&song(128.0, 40, 24)).unwrap();
        let start = a.window_start(25.0);
        let rel = (a.drop_time() - start) / 25.0;
        assert!(
            (0.2..=0.45).contains(&rel),
            "drop at {rel:.2} of the montage"
        );
    }
}
