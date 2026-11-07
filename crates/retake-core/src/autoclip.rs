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
    /// Luma above which a pixel counts as text or icon (kill rows are white text on a box).
    pub bright: u8,
    /// Share of a scanline's pixels that must be bright for it to belong to a row.
    pub min_fill: f64,
    /// Rows shorter than this share of the region height are noise.
    pub min_height: f64,
    /// Rows taller than this share of the region height aren't kill rows.
    pub max_height: f64,
    /// Bright/dark transitions a scanline needs per 100 px: text has many, scenery few.
    pub min_transitions: f64,
    /// A kill row reaches this close to the region's right edge (share of its width).
    pub right_margin: f64,
    /// Mean absolute difference (0–255) under which two rows are the same row.
    pub same: f64,
    /// Seconds after a new row during which another new row at the same height is ignored.
    pub min_gap: f64,
}

impl Default for RowParams {
    fn default() -> Self {
        RowParams {
            bright: 170,
            min_fill: 0.02,
            min_height: 0.04,
            max_height: 0.16,
            min_transitions: 4.0,
            right_margin: 0.40,
            same: 22.0,
            min_gap: 0.6,
        }
    }
}

/// Columns in a row's fingerprint.
const SIG_COLS: usize = 48;

/// A text row found in one frame, by what it looks like.
#[derive(Debug, Clone)]
struct Row {
    sig: Vec<f64>,
}

/// The rows of text in a kill-feed frame: runs of scanlines with enough bright pixels, each
/// with a fingerprint (mean luma of `SIG_COLS` column strips over the row's height).
fn rows_of(f: &FeedFrame<'_>, p: &RowParams) -> Vec<Row> {
    let (w, h) = (f.width, f.height);
    if w == 0 || h == 0 || f.luma.len() < w * h {
        return Vec::new();
    }
    let need_edges = p.min_transitions * w as f64 / 100.0;
    let right_zone = w - ((w as f64 * p.right_margin) as usize).max(1);
    let filled: Vec<bool> = (0..h)
        .map(|y| {
            let line = &f.luma[y * w..(y + 1) * w];
            let bright = line.iter().filter(|&&v| v >= p.bright).count() as f64 / w as f64;
            let edges = line
                .windows(2)
                .filter(|v| (v[0] >= p.bright) != (v[1] >= p.bright))
                .count() as f64;
            let reaches_right = line[right_zone..].iter().any(|&v| v >= p.bright);
            bright >= p.min_fill && edges >= need_edges && reaches_right
        })
        .collect();
    let min_h = ((h as f64 * p.min_height).round() as usize).max(2);
    let max_h = (h as f64 * p.max_height).round() as usize;
    let mut rows = Vec::new();
    let mut y = 0;
    while y < h {
        if !filled[y] {
            y += 1;
            continue;
        }
        let start = y;
        while y < h && filled[y] {
            y += 1;
        }
        // Scanlines inside a row may miss the edge test (gaps between letters); a row is the
        // run of qualifying lines, which must have a kill row's height.
        if y - start >= min_h && y - start <= max_h {
            let sig = (0..SIG_COLS)
                .map(|c| {
                    let (x0, x1) = (
                        c * w / SIG_COLS,
                        ((c + 1) * w / SIG_COLS).max(c * w / SIG_COLS + 1),
                    );
                    let mut sum = 0u64;
                    for yy in start..y {
                        sum += f.luma[yy * w + x0..yy * w + x1]
                            .iter()
                            .map(|&v| u64::from(v))
                            .sum::<u64>();
                    }
                    sum as f64 / ((x1 - x0) * (y - start)) as f64
                })
                .collect();
            rows.push(Row { sig });
        }
    }
    rows
}

fn sig_diff(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>() / a.len().max(1) as f64
}

/// Times a new row appeared in the kill feed. Siege adds rows below the ones already shown and
/// drops old rows from the top, with system messages mixed in, so a row is new when nothing in
/// the previous frame looks like it, wherever it sits. Rows that merely move up don't count.
pub fn new_rows(frames: &[FeedFrame<'_>], p: &RowParams) -> Vec<f64> {
    let mut out: Vec<f64> = Vec::new();
    let mut prev: Vec<Row> = Vec::new();
    // Rows reported moments ago, by fingerprint: a row fading in changes a little from frame
    // to frame and must not count twice.
    let mut recent: Vec<(f64, Vec<f64>)> = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        // The first frame is the baseline: rows already showing aren't new.
        let first = i == 0;
        let rows = rows_of(f, p);
        for r in &rows {
            let seen = prev.iter().any(|q| sig_diff(&q.sig, &r.sig) < p.same);
            let near_recent = recent
                .iter()
                .any(|(t, s)| f.time - t < p.min_gap && sig_diff(s, &r.sig) < 2.0 * p.same);
            if !first && !seen && !near_recent {
                out.push(f.time);
                recent.push((f.time, r.sig.clone()));
            }
        }
        recent.retain(|(t, _)| f.time - t < p.min_gap);
        prev = rows;
    }
    out.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    out
}
/// Siege's kill feed as fractions of the frame *height* (the HUD scales with height and hugs
/// the right edge, so this fits 16:9, 16:10 and 21:9 alike): `(width, top, height)`; the
/// region's right edge is the frame's.
pub const SIEGE_FEED: (f64, f64, f64) = (0.462, 0.22, 0.26);

/// The kill-feed crop `(w, h, x, y)` in pixels for a `width × height` recording.
pub fn feed_crop(width: u32, height: u32) -> (u32, u32, u32, u32) {
    let (fw, top, fh) = SIEGE_FEED;
    let h = f64::from(height);
    let w = ((fw * h) as u32).min(width) / 2 * 2;
    let ch = ((fh * h) as u32) / 2 * 2;
    (w, ch, width - w, (top * h) as u32)
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

    const W: usize = 96;
    const H: usize = 100; // 10 slots of 10 px

    /// A feed whose slots (top first) hold these row ids; 0 = empty. Each id has its own
    /// pattern of bright "text" on a dark box.
    fn feed(rows: &[u8]) -> Vec<u8> {
        let mut v = vec![10u8; W * H];
        for (k, &id) in rows.iter().enumerate() {
            if id == 0 {
                continue;
            }
            for y in k * 10 + 2..k * 10 + 8 {
                for x in 0..W {
                    let bright = (x * (id as usize) + id as usize * 3) % 7 < 3;
                    v[y * W + x] = if bright { 230 } else { 50 };
                }
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
    fn rows_added_below_are_detected_once_each() {
        let seq = vec![
            feed(&[]),
            feed(&[1]), // first row
            feed(&[1]),
            feed(&[1, 2]), // a second row below it
            feed(&[1, 2]),
            feed(&[2, 3]), // the oldest expired, rows moved up, a third appeared
            feed(&[2, 3]),
        ];
        assert_eq!(detect(&seq), [0.25, 0.75, 1.25]);
    }

    #[test]
    fn rows_moving_up_or_fading_are_not_kills() {
        let seq = vec![feed(&[1, 2, 3]), feed(&[2, 3]), feed(&[3]), feed(&[])];
        assert!(detect(&seq).is_empty());
    }

    #[test]
    fn rows_already_there_at_the_start_are_not_new() {
        let seq = vec![feed(&[4, 5]), feed(&[4, 5])];
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
    fn feed_crop_hugs_the_right_edge() {
        assert_eq!(feed_crop(1920, 1080), (498, 280, 1422, 237));
        let (w, _, x, _) = feed_crop(3440, 1440);
        assert_eq!(x + w, 3440);
        assert_eq!(w, 664);
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
