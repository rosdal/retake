//! Automatic kill clips, as pure logic: spotting new kill-feed rows in Retake's own recording,
//! lining a round's replay kills up with those rows, and grouping the player's kills into
//! clips (kill, 2K, 3K, 4K, ACE). Nothing here reads the game; the inputs are files on disk
//! read after the match.

/// One frame of the kill-feed region, greyscale, `width × height`, at `time` seconds into the
/// recording.
pub struct FeedFrame<'a> {
    pub time: f64,
    pub width: usize,
    pub height: usize,
    pub luma: &'a [u8],
}

/// Tuning for [`new_rows`].
#[derive(Debug, Clone, Copy)]
pub struct RowParams {
    /// Height of one kill-feed row as a fraction of the region height.
    pub row_frac: f64,
    /// Mean absolute difference (0–255) above which a band counts as changed.
    pub changed: f64,
    /// Mean absolute difference below which a band counts as the same.
    pub same: f64,
    /// Minimum seconds between two detected rows.
    pub min_gap: f64,
}

impl Default for RowParams {
    fn default() -> Self {
        RowParams {
            row_frac: 0.2,
            changed: 18.0,
            same: 10.0,
            min_gap: 0.4,
        }
    }
}

fn band<'a>(f: &'a FeedFrame<'a>, k: usize, rows: usize) -> impl Iterator<Item = u8> + 'a {
    let h = f.height / rows;
    f.luma[k * h * f.width..((k + 1) * h * f.width).min(f.luma.len())]
        .iter()
        .copied()
}

fn diff(a: impl Iterator<Item = u8>, b: impl Iterator<Item = u8>) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    for (x, y) in a.zip(b) {
        sum += u64::from(x.abs_diff(y));
        n += 1;
    }
    if n == 0 { 0.0 } else { sum as f64 / n as f64 }
}

/// Times a new row appeared at the top of the kill feed: the top band changed while the band
/// below now shows what the top band showed before (the feed shifted down), or the feed was
/// empty below. Fades of existing rows don't count, because nothing shifts.
pub fn new_rows(frames: &[FeedFrame<'_>], p: &RowParams) -> Vec<f64> {
    let rows = ((1.0 / p.row_frac).round() as usize).max(2);
    let mut out: Vec<f64> = Vec::new();
    for w in frames.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        if a.width != b.width || a.height != b.height || a.height < rows {
            continue;
        }
        let top_changed = diff(band(a, 0, rows), band(b, 0, rows)) > p.changed;
        if !top_changed {
            continue;
        }
        // The old top row moved one band down…
        let shifted = diff(band(a, 0, rows), band(b, 1, rows)) < p.same;
        // …or the band below was empty (dark) before and still is: the first row of a feed.
        let band_px = ((a.height / rows) * a.width).max(1) as f64;
        let mean = |f: &FeedFrame<'_>, k: usize| -> f64 {
            band(f, k, rows).map(f64::from).sum::<f64>() / band_px
        };
        let first = mean(a, 0) < p.same && mean(a, 1) < 24.0 && mean(b, 1) < 24.0;
        if (shifted || first) && out.last().is_none_or(|&t| b.time - t >= p.min_gap) {
            out.push(b.time);
        }
    }
    out
}

/// A kill from the replay: seconds since the round started, and whether the player made it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReplayKill {
    pub at: f64,
    pub mine: bool,
}

/// Lines a round's replay kills up with detected kill-feed rows (recording seconds). Tries
/// every kill/row pair as an anchor for a constant offset, matches the rest in order within
/// `tolerance` seconds, and keeps the offset with the most matches (then the least error).
/// Returns, per kill, the recording time it happened, or `None` if no row matched.
pub fn align(kills: &[ReplayKill], rows: &[f64], tolerance: f64) -> Vec<Option<f64>> {
    let mut best: Option<(usize, f64, Vec<Option<f64>>)> = None;
    for k in kills {
        for &r in rows {
            let offset = r - k.at;
            let mut used = 0usize;
            let mut err = 0.0;
            let mut out = Vec::with_capacity(kills.len());
            let mut next_row = 0usize;
            for kk in kills {
                let want = kk.at + offset;
                let m = rows
                    .iter()
                    .enumerate()
                    .skip(next_row)
                    .find(|(_, t)| (**t - want).abs() <= tolerance);
                match m {
                    Some((j, &t)) => {
                        used += 1;
                        err += (t - want).abs();
                        next_row = j + 1;
                        out.push(Some(t));
                    }
                    None => out.push(None),
                }
            }
            let better = match &best {
                None => true,
                Some((u, e, _)) => used > *u || (used == *u && err < *e),
            };
            if better {
                best = Some((used, err, out));
            }
        }
    }
    best.map(|b| b.2).unwrap_or_else(|| vec![None; kills.len()])
}

/// What a clip holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Play {
    Kill,
    Multi(u8),
    Ace,
}

impl Play {
    /// The file-name tag: `kill`, `2K`, `3K`, `4K`, `ACE`.
    pub fn tag(self) -> String {
        match self {
            Play::Kill => "kill".into(),
            Play::Multi(n) => format!("{n}K"),
            Play::Ace => "ACE".into(),
        }
    }

    fn of(kills_in_round: usize, enemies: usize) -> Play {
        match kills_in_round {
            0 | 1 => Play::Kill,
            n if n >= enemies => Play::Ace,
            n => Play::Multi(n as u8),
        }
    }
}

/// Which plays are clipped automatically (`autoclip.clip`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinPlay {
    Every,
    TwoK,
    ThreeK,
    Aces,
    Off,
}

/// A clip to cut from the session recording.
#[derive(Debug, Clone, PartialEq)]
pub struct KillClip {
    /// Recording seconds.
    pub start: f64,
    pub end: f64,
    /// The round's total for the player (a 3K split in two is `Multi(3)` twice).
    pub play: Play,
    /// `(part, parts)` when one round's kills were split; `(1, 1)` otherwise.
    pub part: (usize, usize),
    /// Kill times within the clip.
    pub kills: Vec<f64>,
}

pub const PRE_ROLL: f64 = 15.0;
pub const POST_ROLL: f64 = 3.0;
pub const MAX_CLIP: f64 = 90.0;
/// Kills further apart than this become separate clips.
pub const SPLIT_GAP: f64 = 45.0;

/// The clips for one round from the recording times of the player's kills (sorted), given how
/// many enemies there were (an ACE is all of them).
pub fn round_clips(my_kills: &[f64], enemies: usize, min: MinPlay) -> Vec<KillClip> {
    if my_kills.is_empty() {
        return Vec::new();
    }
    let play = Play::of(my_kills.len(), enemies.max(1));
    let wanted = match min {
        MinPlay::Off => false,
        MinPlay::Every => true,
        MinPlay::TwoK => my_kills.len() >= 2,
        MinPlay::ThreeK => my_kills.len() >= 3,
        MinPlay::Aces => play == Play::Ace,
    };
    if !wanted {
        return Vec::new();
    }
    // Split on long gaps, and so no clip runs past MAX_CLIP.
    let mut groups: Vec<Vec<f64>> = vec![vec![my_kills[0]]];
    for w in my_kills.windows(2) {
        let g = groups.last_mut().expect("never empty");
        let span = w[1] - g[0] + PRE_ROLL + POST_ROLL;
        if w[1] - w[0] > SPLIT_GAP || span > MAX_CLIP {
            groups.push(vec![w[1]]);
        } else {
            g.push(w[1]);
        }
    }
    let parts = groups.len();
    groups
        .into_iter()
        .enumerate()
        .map(|(i, g)| KillClip {
            start: (g[0] - PRE_ROLL).max(0.0),
            end: g[g.len() - 1] + POST_ROLL,
            play,
            part: (i + 1, parts),
            kills: g,
        })
        .collect()
}

/// `YYYY-MM-DD_HHMMSS_<tag>` plus `-partN` when a round was split.
pub fn clip_stem(stamp: &str, c: &KillClip) -> String {
    if c.part.1 > 1 {
        format!("{stamp}_{}-part{}", c.play.tag(), c.part.0)
    } else {
        format!("{stamp}_{}", c.play.tag())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 40;
    const H: usize = 50; // 5 bands of 10 px

    /// A feed whose rows (top first) have these brightness values; 0 = empty.
    fn feed(rows: &[u8]) -> Vec<u8> {
        let mut v = vec![0u8; W * H];
        for (k, &val) in rows.iter().enumerate() {
            for p in &mut v[k * 10 * W..(k + 1) * 10 * W] {
                *p = val;
            }
        }
        v
    }

    fn detect(seq: &[Vec<u8>]) -> Vec<f64> {
        let frames: Vec<FeedFrame<'_>> = seq
            .iter()
            .enumerate()
            .map(|(i, l)| FeedFrame {
                time: i as f64 * 0.25,
                width: W,
                height: H,
                luma: l,
            })
            .collect();
        new_rows(&frames, &RowParams::default())
    }

    #[test]
    fn rows_pushing_down_are_detected_once_each() {
        let seq = vec![
            feed(&[]),
            feed(&[200]), // first row on an empty feed
            feed(&[200]),
            feed(&[120, 200]), // new row, old one shifted down
            feed(&[120, 200]),
            feed(&[90, 120, 200]), // another
        ];
        assert_eq!(detect(&seq), [0.25, 0.75, 1.25]);
    }

    #[test]
    fn a_row_fading_out_is_not_a_kill() {
        let seq = vec![feed(&[120, 200]), feed(&[120, 60]), feed(&[120])];
        assert!(detect(&seq).is_empty());
    }

    fn k(at: f64, mine: bool) -> ReplayKill {
        ReplayKill { at, mine }
    }

    #[test]
    fn align_with_an_offset_and_a_missing_row() {
        let kills = [k(10.0, true), k(14.0, false), k(30.0, true)];
        // Recording is 100 s ahead; the second kill's row was missed; an extra row at 120.
        let rows = [110.2, 120.0, 129.9];
        let a = align(&kills, &rows, 0.5);
        assert_eq!(a, [Some(110.2), None, Some(129.9)]);
    }

    #[test]
    fn align_trades_within_a_second() {
        let kills = [k(5.0, true), k(5.6, false), k(20.0, true)];
        let rows = [65.0, 65.6, 80.1];
        assert_eq!(
            align(&kills, &rows, 0.5),
            [Some(65.0), Some(65.6), Some(80.1)]
        );
    }

    #[test]
    fn align_nothing_matches() {
        assert_eq!(align(&[k(1.0, true)], &[], 0.5), [None]);
    }

    #[test]
    fn a_3k_spray_is_one_clip() {
        let c = round_clips(&[100.0, 102.0, 105.0], 5, MinPlay::Every);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].play, Play::Multi(3));
        assert_eq!((c[0].start, c[0].end), (85.0, 108.0));
        assert_eq!(
            clip_stem("2026-11-07_201500", &c[0]),
            "2026-11-07_201500_3K"
        );
    }

    #[test]
    fn long_gaps_split_but_keep_the_round_total() {
        let c = round_clips(&[10.0, 70.0], 5, MinPlay::Every);
        assert_eq!(c.len(), 2);
        assert!(c.iter().all(|x| x.play == Play::Multi(2)));
        assert_eq!(clip_stem("s", &c[1]), "s_2K-part2");
    }

    #[test]
    fn ace_and_filters() {
        let five = [1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(round_clips(&five, 5, MinPlay::Aces)[0].play, Play::Ace);
        assert!(round_clips(&[1.0], 5, MinPlay::TwoK).is_empty());
        assert!(round_clips(&[1.0, 2.0], 5, MinPlay::ThreeK).is_empty());
        assert!(round_clips(&five, 5, MinPlay::Off).is_empty());
        assert_eq!(
            round_clips(&[50.0], 5, MinPlay::Every)[0].play.tag(),
            "kill"
        );
        // Start never goes before the recording.
        assert_eq!(round_clips(&[5.0], 5, MinPlay::Every)[0].start, 0.0);
    }

    #[test]
    fn no_clip_longer_than_ninety_seconds() {
        let kills: Vec<f64> = (0..8).map(|i| 100.0 + f64::from(i) * 12.0).collect();
        for c in round_clips(&kills, 5, MinPlay::Every) {
            assert!(c.end - c.start <= MAX_CLIP + 1e-9, "{c:?}");
        }
    }
}
