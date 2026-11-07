//! Hotkey presses turned into clip jobs: the window each clip covers, merging presses that land
//! in a pending clip's post-roll, and the file name and ffmpeg command for writing one. Pure.

use std::path::{Path, PathBuf};

use crate::Ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `pre_roll` before the press to `post_roll` after it.
    Normal,
    /// The whole buffer up to `post_roll` after the press.
    Full,
}

/// Local date and time of a press, for the file name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Press {
    /// Monotonic ms (the same clock as the session start).
    pub at: Ms,
    pub stamp: Stamp,
    pub kind: Kind,
}

/// One clip to write: window `[from, to]` in clock ms, named after its first press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipJob {
    pub id: u64,
    pub kind: Kind,
    pub press: Ms,
    pub stamp: Stamp,
    pub from: Ms,
    pub to: Ms,
}

/// A clip asked for after recording stopped (the game closed or crashed) would mostly cover
/// time with nothing recorded. It is moved back to end where the recording ended, so F9 right
/// after a crash still saves the moment of the crash.
pub fn after_stop(job: &ClipJob, recording_end: Ms) -> ClipJob {
    let mut j = job.clone();
    if j.to > recording_end {
        let shift = j.to - recording_end;
        j.to = recording_end;
        j.from = j.from.saturating_sub(shift);
        j.press = j.press.saturating_sub(shift);
    }
    j
}

/// How much shorter than the buffer a merged clip must stay: the segment being written plus
/// the wait for the last one to close (the same 4 s `config` requires beyond the rolls).
pub const MERGE_MARGIN: Ms = 4_000;

/// Pending clips, waiting out their post-roll.
pub struct Scheduler {
    pre: Ms,
    post: Ms,
    full_pre: Ms,
    next_id: u64,
    pending: Vec<ClipJob>,
}

impl Scheduler {
    /// New pre/post roll (seconds) for presses from now on.
    pub fn set_rolls(&mut self, pre_roll: u32, post_roll: u32) {
        self.pre = Ms::from(pre_roll) * 1000;
        self.post = Ms::from(post_roll) * 1000;
    }

    /// `pre_roll` and `post_roll` from `[clips]` and `buffer_secs` from `[capture]`, in seconds.
    pub fn new(pre_roll: u32, post_roll: u32, buffer_secs: u32) -> Scheduler {
        let ms = |s: u32| Ms::from(s) * 1000;
        Scheduler {
            pre: ms(pre_roll),
            post: ms(post_roll),
            full_pre: ms(buffer_secs),
            next_id: 0,
            pending: Vec::new(),
        }
    }

    /// Records a press. A normal press inside a pending normal clip's post-roll moves that
    /// clip's end to `press + post_roll` and returns true, unless the clip would then span more
    /// than the buffer minus [`MERGE_MARGIN`] (its start would be overwritten before it is
    /// saved); anything else starts a new clip.
    pub fn press(&mut self, p: Press) -> bool {
        let max_span = self.full_pre.saturating_sub(MERGE_MARGIN);
        let to = p.at + self.post;
        if p.kind == Kind::Normal
            && let Some(job) = self
                .pending
                .iter_mut()
                .find(|j| j.kind == Kind::Normal && p.at < j.to && to - j.from <= max_span)
        {
            job.to = job.to.max(p.at + self.post);
            return true;
        }
        let pre = match p.kind {
            Kind::Normal => self.pre,
            Kind::Full => self.full_pre,
        };
        self.pending.push(ClipJob {
            id: self.next_id,
            kind: p.kind,
            press: p.at,
            stamp: p.stamp,
            from: p.at.saturating_sub(pre),
            to: p.at + self.post,
        });
        self.next_id += 1;
        false
    }

    /// Removes and returns the clips whose post-roll is over at `now`, oldest first.
    pub fn due(&mut self, now: Ms) -> Vec<ClipJob> {
        let (due, wait): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(|j| now >= j.to);
        self.pending = wait;
        due
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }
}

/// `YYYY-MM-DD_HHMMSS.mkv`, with `_full` before the extension for a full-buffer clip.
pub fn clip_name(t: Stamp, kind: Kind) -> String {
    let suffix = match kind {
        Kind::Normal => "",
        Kind::Full => "_full",
    };
    format!(
        "{:04}-{:02}-{:02}_{:02}{:02}{:02}{suffix}.mkv",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    )
}

/// `name` with `_<n>` before its extension, for a name that is already taken.
pub fn numbered(name: &str, n: u32) -> String {
    match name.rsplit_once('.') {
        Some((stem, ext)) => format!("{stem}_{n}.{ext}"),
        None => format!("{name}_{n}"),
    }
}

/// The concat demuxer's list for `files`, named relative to the list's own folder.
pub fn concat_list(files: &[String]) -> String {
    files
        .iter()
        .map(|f| format!("file '{}'\n", f.replace('\'', "'\\''")))
        .collect()
}

/// Joins the listed segments without re-encoding into Matroska at `out`. The format is given
/// explicitly because `out` ends in `.part`.
/// The file names in a list written by [`concat_list`], in order.
pub fn parse_concat_list(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("file '")?.strip_suffix('\''))
        .map(|f| f.replace("'\\''", "'"))
        .collect()
}

fn concat_url(files: &[PathBuf]) -> String {
    let parts: Vec<String> = files
        .iter()
        .map(|f| f.to_string_lossy().into_owned())
        .collect();
    format!("concat:{}", parts.join("|"))
}

/// Joins MPEG-TS segments byte for byte (the `concat:` protocol, which suits TS) into
/// Matroska, stream copy, timestamps starting at 0.
pub fn ts_join_args(files: &[PathBuf], out: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-y",
        "-i",
    ]
    .map(String::from)
    .to_vec();
    a.push(concat_url(files));
    a.extend(["-map", "0", "-c", "copy", "-f", "matroska"].map(String::from));
    a.push(out.to_string_lossy().into_owned());
    a
}

/// Joins video segments with audio segments from the separate audio ring. Both rings keep
/// timestamps counted from the same origin, so `-copyts` lines them up exactly; one common
/// shift then moves the earliest packet to 0.
pub fn ts_mux_args(video: &[PathBuf], audio: &[PathBuf], out: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-y",
        "-copyts",
        "-i",
    ]
    .map(String::from)
    .to_vec();
    a.push(concat_url(video));
    a.push("-i".into());
    a.push(concat_url(audio));
    a.extend(
        [
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-c",
            "copy",
            "-output_ts_offset",
            "-100000",
            "-avoid_negative_ts",
            "make_zero",
            "-f",
            "matroska",
        ]
        .map(String::from),
    );
    a.push(out.to_string_lossy().into_owned());
    a
}
pub fn concat_args(list: &Path, out: &Path) -> Vec<String> {
    let mut a: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "warning",
        "-nostdin",
        "-y",
        "-f",
        "concat",
        "-safe",
        "0",
        "-i",
    ]
    .map(String::from)
    .to_vec();
    a.push(list.to_string_lossy().into_owned());
    a.extend(["-c", "copy", "-map", "0", "-f", "matroska"].map(String::from));
    a.push(out.to_string_lossy().into_owned());
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Stamp = Stamp {
        year: 2026,
        month: 11,
        day: 6,
        hour: 20,
        minute: 15,
        second: 33,
    };

    fn press(at: Ms, kind: Kind) -> Press {
        Press { at, stamp: T, kind }
    }

    fn sched() -> Scheduler {
        Scheduler::new(15, 3, 60)
    }

    #[test]
    fn one_press_is_due_after_the_post_roll() {
        let mut s = sched();
        assert!(!s.press(press(100_000, Kind::Normal)));
        assert!(s.due(102_999).is_empty());
        let job = ClipJob {
            id: 0,
            kind: Kind::Normal,
            press: 100_000,
            stamp: T,
            from: 85_000,
            to: 103_000,
        };
        assert_eq!(s.due(103_000), [job]);
        assert_eq!(s.pending(), 0);
    }

    #[test]
    fn presses_in_the_post_roll_extend_one_clip() {
        let mut s = sched();
        for at in [100_000, 100_500, 101_000, 101_500, 102_000] {
            s.press(press(at, Kind::Normal));
        }
        assert!(s.due(104_999).is_empty());
        let jobs = s.due(105_000);
        assert_eq!(jobs.len(), 1);
        assert_eq!(
            (jobs[0].press, jobs[0].from, jobs[0].to),
            (100_000, 85_000, 105_000)
        );
    }

    #[test]
    fn press_at_the_end_starts_a_new_clip() {
        let mut s = sched();
        s.press(press(100_000, Kind::Normal));
        assert!(!s.press(press(103_000, Kind::Normal)));
        assert_eq!(s.pending(), 2);
    }

    #[test]
    fn full_clips_cover_the_buffer_and_never_merge() {
        let mut s = sched();
        s.press(press(100_000, Kind::Full));
        assert!(!s.press(press(101_000, Kind::Normal)));
        assert!(!s.press(press(101_500, Kind::Full)));
        let jobs = s.due(200_000);
        assert_eq!(jobs.iter().map(|j| j.id).collect::<Vec<_>>(), [0, 1, 2]);
        assert_eq!(
            (jobs[0].kind, jobs[0].from, jobs[0].to),
            (Kind::Full, 40_000, 103_000)
        );
    }

    #[test]
    fn merging_stops_before_the_clip_outgrows_the_buffer() {
        // 30 s buffer: a merged clip may span at most 26 s (buffer − 4 s), so its start is
        // still in the ring when it is saved.
        let mut s = Scheduler::new(15, 3, 30);
        s.press(press(100_000, Kind::Normal)); // 85–103 s
        for at in [102_000, 104_000, 106_000] {
            assert!(s.press(press(at, Kind::Normal)));
        }
        assert!(s.press(press(108_000, Kind::Normal))); // 85–111 s = 26 s
        assert!(!s.press(press(110_000, Kind::Normal))); // would be 28 s: a new clip
        let jobs = s.due(200_000);
        assert_eq!(
            jobs.iter().map(|j| (j.from, j.to)).collect::<Vec<_>>(),
            [(85_000, 111_000), (95_000, 113_000)]
        );
    }

    #[test]
    fn early_press_window_starts_at_zero() {
        let mut s = sched();
        s.press(press(5_000, Kind::Normal));
        assert_eq!(s.due(8_000)[0].from, 0);
    }

    #[test]
    fn names() {
        assert_eq!(clip_name(T, Kind::Normal), "2026-11-06_201533.mkv");
        assert_eq!(clip_name(T, Kind::Full), "2026-11-06_201533_full.mkv");
        assert_eq!(
            numbered("2026-11-06_201533.mkv", 2),
            "2026-11-06_201533_2.mkv"
        );
    }

    #[test]
    fn concat_list_quotes_names() {
        let l = concat_list(&["000.ts".into(), "it's.ts".into()]);
        assert_eq!(l, "file '000.ts'\nfile 'it'\\''s.ts'\n");
    }

    #[test]
    fn concat_command() {
        let a = concat_args(Path::new(r"C:\t\list.txt"), Path::new(r"C:\c\a.mkv.part"));
        assert_eq!(
            a,
            [
                "-hide_banner",
                "-loglevel",
                "warning",
                "-nostdin",
                "-y",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
                r"C:\t\list.txt",
                "-c",
                "copy",
                "-map",
                "0",
                "-f",
                "matroska",
                r"C:\c\a.mkv.part",
            ]
        );
    }
}

#[cfg(test)]
mod mux_tests {
    use super::*;

    #[test]
    fn list_round_trips_and_join_uses_the_concat_protocol() {
        let names = vec!["000.ts".to_string(), "it's.ts".to_string()];
        assert_eq!(parse_concat_list(&concat_list(&names)), names);
        let a = ts_join_args(
            &[PathBuf::from(r"D:\w\000.ts"), PathBuf::from(r"D:\w\001.ts")],
            Path::new("o.mkv"),
        );
        assert!(a.contains(&r"concat:D:\w\000.ts|D:\w\001.ts".to_string()));
    }

    #[test]
    fn a_press_after_the_recording_ended_saves_its_last_seconds() {
        let job = ClipJob {
            id: 1,
            kind: Kind::Normal,
            press: 100_000,
            stamp: Stamp {
                year: 2026,
                month: 11,
                day: 7,
                hour: 1,
                minute: 0,
                second: 0,
            },
            from: 85_000,
            to: 103_000,
        };
        let j = after_stop(&job, 80_000);
        assert_eq!((j.from, j.to, j.press), (62_000, 80_000, 77_000));
        // Recording still covers the clip: unchanged.
        assert_eq!(after_stop(&job, 200_000), job);
    }

    #[test]
    fn mux_keeps_both_timelines() {
        let a = ts_mux_args(
            &[PathBuf::from("v.ts")],
            &[PathBuf::from("a.ts")],
            Path::new("o.mkv"),
        );
        assert!(a.contains(&"-copyts".to_string()));
        assert!(
            a.windows(2)
                .any(|w| w == ["-avoid_negative_ts", "make_zero"])
        );
        assert!(a.windows(4).any(|w| w == ["-map", "0:v", "-map", "1:a"]));
    }
}
