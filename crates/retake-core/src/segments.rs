//! The ring buffer's `index.csv` read as segments on Retake's clock, and the choice of which
//! segments make a clip. Pure; times are milliseconds on the same monotonic clock as presses.

use crate::Ms;
use crate::ffmpeg_args::SEGMENT_SECS;

/// One closed segment of the ring, on Retake's clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub file: String,
    pub start: Ms,
    pub end: Ms,
}

pub const SEGMENT_MS: Ms = SEGMENT_SECS as Ms * 1000;
/// The largest gap between one segment's end and the next one's start that still counts as
/// contiguous.
pub const GAP_TOLERANCE: Ms = 500;

/// Parses ffmpeg's CSV segment list (`seg_03.ts,16.000000,18.000000` per line). Its times count
/// from the start of the ffmpeg run; adding `session_start` puts them on Retake's clock. Lines
/// that don't parse, such as a half-written last line, are skipped. Sorted by start.
pub fn parse_index(text: &str, session_start: Ms) -> Vec<Segment> {
    let secs = |s: &str| {
        s.trim()
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(|v| (v * 1000.0).round() as Ms)
    };
    let mut segs: Vec<Segment> = text
        .lines()
        .filter_map(|line| {
            let mut f = line.trim().split(',');
            let (file, start, end) = (f.next()?, f.next()?, f.next()?);
            if f.next().is_some() || file.is_empty() {
                return None;
            }
            let (start, end) = (secs(start)?, secs(end)?);
            (end > start).then(|| Segment {
                file: file.to_owned(),
                start: session_start + start,
                end: session_start + end,
            })
        })
        .collect();
    segs.sort_by_key(|s| s.start);
    segs
}

/// The ring slot of `seg_<n>.ts`.
pub fn slot(file: &str) -> Option<u32> {
    file.strip_prefix("seg_")?.strip_suffix(".ts")?.parse().ok()
}

/// The file ffmpeg is writing now: the slot after the newest listed segment. Once the ring is
/// full, that slot's listed entry is an old segment already being overwritten.
pub fn writing_file(segs: &[Segment], wrap: u32) -> Option<String> {
    let newest = slot(&segs.iter().max_by_key(|s| s.start)?.file)?;
    Some(format!("seg_{:02}.ts", (newest + 1) % wrap))
}

/// The segments for a clip covering `[from, to]` around `press`: every segment overlapping the
/// window except `writing`, in time order. If they don't run back to back (ffmpeg restarted
/// between them), only the contiguous run around `press` is kept: the one containing it, else
/// the last one starting before it, else the first.
pub fn select(
    segs: &[Segment],
    press: Ms,
    from: Ms,
    to: Ms,
    writing: Option<&str>,
) -> Vec<Segment> {
    let mut hit: Vec<&Segment> = segs
        .iter()
        .filter(|s| s.end > from && s.start < to && Some(s.file.as_str()) != writing)
        .collect();
    hit.sort_by_key(|s| s.start);
    let mut runs: Vec<Vec<Segment>> = Vec::new();
    for s in hit {
        match runs.last_mut() {
            Some(run) if s.start <= run[run.len() - 1].end + GAP_TOLERANCE => run.push(s.clone()),
            _ => runs.push(vec![s.clone()]),
        }
    }
    let span = |r: &[Segment]| (r[0].start, r[r.len() - 1].end);
    let pick = runs
        .iter()
        .position(|r| {
            let (a, b) = span(r);
            a <= press && press < b
        })
        .or_else(|| runs.iter().rposition(|r| span(r).0 <= press))
        .unwrap_or(0);
    runs.into_iter().nth(pick).unwrap_or_default()
}

/// For each of `copied` (as listed before copying), whether it is certainly unchanged according
/// to `after`, the index re-read once copying finished: still listed with the same times and
/// not the file being written now. Anything else may have been overwritten mid-copy.
pub fn still_intact(copied: &[Segment], after: &[Segment], wrap: u32) -> Vec<bool> {
    let writing = writing_file(after, wrap);
    copied
        .iter()
        .map(|c| Some(&c.file) != writing.as_ref() && after.contains(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` contiguous 2 s segments starting at `start_s` seconds, slots counting from
    /// `first_slot` and wrapping at `wrap`.
    fn ring(n: u32, wrap: u32, first_slot: u32, start_s: u64) -> Vec<Segment> {
        (0..n)
            .map(|i| {
                let s = (start_s + 2 * u64::from(i)) * 1000;
                Segment {
                    file: format!("seg_{:02}.ts", (first_slot + i) % wrap),
                    start: s,
                    end: s + 2000,
                }
            })
            .collect()
    }

    fn files(segs: &[Segment]) -> Vec<&str> {
        segs.iter().map(|s| s.file.as_str()).collect()
    }

    #[test]
    fn parses_index_onto_the_wall_clock() {
        let text = "seg_03.ts,16.000000,18.000000\nseg_04.ts,18.000000,20.000000\n\
                    seg_00.ts,20.000000,22.000000\r\nnot a line\n\nseg_01.ts,22.0";
        let segs = parse_index(text, 1_000_000);
        assert_eq!(files(&segs), ["seg_03.ts", "seg_04.ts", "seg_00.ts"]);
        assert_eq!((segs[0].start, segs[0].end), (1_016_000, 1_018_000));
        assert_eq!(segs[2].end, 1_022_000);
    }

    #[test]
    fn rejects_nonsense_rows() {
        let text = "seg_00.ts,4.0,2.0\n,0.0,2.0\nseg_01.ts,-2.0,0.0\nseg_02.ts,NaN,2.0";
        assert!(parse_index(text, 0).is_empty());
    }

    #[test]
    fn slot_numbers() {
        assert_eq!(slot("seg_07.ts"), Some(7));
        assert_eq!(slot("seg_123.ts"), Some(123));
        assert_eq!(slot("index.csv"), None);
        assert_eq!(slot("seg_.ts"), None);
    }

    #[test]
    fn writing_file_is_the_slot_after_the_newest() {
        assert_eq!(
            writing_file(&ring(5, 5, 3, 16), 5).as_deref(),
            Some("seg_03.ts")
        );
        assert_eq!(
            writing_file(&ring(3, 30, 27, 0), 30).as_deref(),
            Some("seg_00.ts")
        );
        assert_eq!(writing_file(&[], 30), None);
    }

    #[test]
    fn normal_press_takes_the_window_rounded_out() {
        let segs = ring(30, 30, 0, 0);
        let got = select(&segs, 40_000, 25_000, 43_000, None);
        assert_eq!((got[0].start, got.last().unwrap().end), (24_000, 44_000));
        assert_eq!(got.len(), 10);
        let aligned = select(&segs, 40_000, 26_000, 44_000, None);
        assert_eq!(aligned.len(), 9);
    }

    #[test]
    fn early_press_takes_what_exists() {
        let segs = ring(5, 30, 0, 0);
        let got = select(&segs, 4_000, 0, 7_000, None);
        assert_eq!(
            files(&got),
            ["seg_00.ts", "seg_01.ts", "seg_02.ts", "seg_03.ts"]
        );
    }

    #[test]
    fn wrap_keeps_time_order() {
        let segs = ring(30, 30, 10, 100);
        let got = select(&segs, 144_000, 140_000, 148_000, None);
        assert_eq!(
            files(&got),
            ["seg_00.ts", "seg_01.ts", "seg_02.ts", "seg_03.ts"]
        );
        let wrapped = select(&segs, 138_000, 136_000, 141_000, None);
        assert_eq!(files(&wrapped), ["seg_28.ts", "seg_29.ts", "seg_00.ts"]);
    }

    #[test]
    fn segment_being_written_is_left_out() {
        let segs = ring(30, 30, 0, 0);
        let got = select(&segs, 59_000, 0, 62_000, Some("seg_00.ts"));
        assert_eq!(got.len(), 29);
        assert_eq!(got[0].file, "seg_01.ts");
    }

    #[test]
    fn gap_keeps_the_part_with_the_press() {
        let mut segs = ring(5, 30, 0, 0); // 0–10 s
        segs.extend(ring(5, 30, 5, 20)); // 20–30 s after a restart gap
        let late = select(&segs, 25_000, 10_000, 28_000, None);
        assert_eq!((late[0].start, late.last().unwrap().end), (20_000, 28_000));
        let early = select(&segs, 5_000, 0, 30_000, None);
        assert_eq!((early[0].start, early.last().unwrap().end), (0, 10_000));
        let in_gap = select(&segs, 15_000, 0, 30_000, None);
        assert_eq!((in_gap[0].start, in_gap.last().unwrap().end), (0, 10_000));
    }

    #[test]
    fn intact_means_still_listed_unchanged_and_not_being_written() {
        let copied = ring(3, 30, 0, 0); // seg_00 0–2, seg_01 2–4, seg_02 4–6
        let mut after = ring(30, 30, 1, 2); // seg_00 rolled out and is listed again later
        after[0].end += 1; // seg_01 listed with different times: rewritten
        assert_eq!(still_intact(&copied, &after, 30), [false, false, true]);
        let being_written = ring(29, 30, 3, 6); // newest seg_01 → writing seg_02
        let mut listed = copied.clone();
        listed.extend(being_written);
        assert!(!still_intact(&copied, &listed, 30)[2]);
    }
}
