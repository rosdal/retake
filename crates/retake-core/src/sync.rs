//! A/V sync measurement: pairs bright flashes in the video (muzzle flashes, or the `avsync`
//! stand-in's white frames) with loud onsets in an audio track (gunshots, clicks) and reports
//! the offsets. Rough but objective. Positive offset = the sound comes after the picture.

/// Times where a brightness series rises from below `low` to above `high`. `samples` are
/// `(time, brightness)` in time order.
pub fn rising_edges(samples: &[(f64, f64)], high: f64, low: f64) -> Vec<f64> {
    let mut out = Vec::new();
    let mut armed = false;
    for &(t, y) in samples {
        if y < low {
            armed = true;
        } else if y > high && armed {
            out.push(t);
            armed = false;
        }
    }
    out
}

/// Times (from `start`, in seconds) of the first sample louder than `loud` after at least
/// `quiet_secs` of samples that are not. (A sound's own rising edge stays below `loud` for a
/// few samples; that must not count against the quiet before it.)
pub fn onsets(samples: &[i16], rate: u32, start: f64, loud: i16, quiet_secs: f64) -> Vec<f64> {
    let need = (quiet_secs * f64::from(rate)) as usize;
    let mut out = Vec::new();
    let mut quiet = need; // the very start counts as quiet
    for (i, &s) in samples.iter().enumerate() {
        if s.unsigned_abs() > loud.unsigned_abs() {
            if quiet >= need {
                out.push(start + i as f64 / f64::from(rate));
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    out
}

/// For each flash with a sound within `max_gap` seconds: `(flash time, offset in ms)` to the
/// nearest such sound.
pub fn pair(flashes: &[f64], sounds: &[f64], max_gap: f64) -> Vec<(f64, f64)> {
    flashes
        .iter()
        .filter_map(|&f| {
            sounds
                .iter()
                .map(|&s| s - f)
                .filter(|d| d.abs() <= max_gap)
                .min_by(|a, b| a.abs().total_cmp(&b.abs()))
                .map(|d| (f, d * 1000.0))
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyncReport {
    pub pairs: usize,
    pub median_ms: f64,
    pub worst_ms: f64,
}

/// Median and worst absolute offset; `None` without pairs.
pub fn report(offsets: &[f64]) -> Option<SyncReport> {
    if offsets.is_empty() {
        return None;
    }
    let mut v = offsets.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let median = if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    };
    Some(SyncReport {
        pairs: n,
        median_ms: median,
        worst_ms: v.iter().fold(0.0, |m: f64, o| m.max(o.abs())),
    })
}

/// The pass rule for `retake sync-check`: median within ±8 ms (one frame at 120 fps) and no
/// pair beyond 50 ms.
pub fn passes(r: &SyncReport) -> bool {
    r.median_ms.abs() <= 8.0 && r.worst_ms <= 50.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_need_a_dark_frame_between() {
        let s = [
            (0.0, 10.0),
            (0.1, 200.0),
            (0.2, 210.0),
            (0.3, 100.0), // dims but not dark: no new edge
            (0.4, 220.0),
            (0.5, 20.0),
            (0.6, 230.0),
        ];
        assert_eq!(rising_edges(&s, 128.0, 64.0), [0.1, 0.6]);
    }

    #[test]
    fn onsets_follow_silence() {
        let mut s = vec![0i16; 48_000];
        s[4_800] = 10_000; // after 100 ms of quiet: counts
        s[4_900] = 10_000; // 2 ms later: same sound
        s[30_000] = 10_000; // after ~520 ms of quiet: counts
        let o = onsets(&s, 48_000, 1.0, 3_000, 0.3);
        assert_eq!(o.len(), 2, "{o:?}");
        assert!((o[0] - 1.1).abs() < 1e-9);
        assert!((o[1] - (1.0 + 30_000.0 / 48_000.0)).abs() < 1e-9);
    }

    #[test]
    fn a_rising_edge_does_not_hide_the_onset() {
        let mut s = vec![0i16; 48_000];
        // A 1 kHz burst: its first samples are quiet-ish, then loud.
        for (i, v) in s[24_000..24_480].iter_mut().enumerate() {
            *v = (12_000.0 * (i as f32 * 2.0 * std::f32::consts::PI / 48.0).sin()) as i16;
        }
        let o = onsets(&s, 48_000, 0.0, 3_000, 0.3);
        assert_eq!(o.len(), 1);
        assert!((o[0] - 0.5).abs() < 0.001, "{o:?}");
    }

    #[test]
    fn pairs_take_the_nearest_sound_within_reach() {
        let p = pair(&[1.0, 2.0, 3.0], &[1.010, 1.5, 2.995, 9.0], 0.1);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].0, 1.0);
        assert!((p[0].1 - 10.0).abs() < 1e-6);
        assert_eq!(p[1].0, 3.0);
        assert!((p[1].1 + 5.0).abs() < 1e-6);
    }

    #[test]
    fn report_median_and_worst() {
        let r = report(&[3.0, -1.0, 10.0, 2.0]).unwrap();
        assert_eq!(r.pairs, 4);
        assert!((r.median_ms - 2.5).abs() < 1e-9);
        assert!((r.worst_ms - 10.0).abs() < 1e-9);
        assert!(passes(&r));
        assert!(!passes(&report(&[9.0, 9.5, 10.0]).unwrap()));
        assert!(!passes(&report(&[0.0, 0.0, 60.0]).unwrap()));
        assert!(report(&[]).is_none());
    }
}
