//! Hotkey presses turned into clip jobs: the window each clip covers, merging presses that land
//! in a pending clip's post-roll, and the file name and ffmpeg command for writing one. Pure.

use std::path::Path;

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
    /// Wall-clock ms since the Unix epoch.
    pub at: Ms,
    pub stamp: Stamp,
    pub kind: Kind,
}

/// One clip to write: wall-clock window `[from, to]`, named after its first press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipJob {
    pub id: u64,
    pub kind: Kind,
    pub press: Ms,
    pub stamp: Stamp,
    pub from: Ms,
    pub to: Ms,
}

/// Pending clips, waiting out their post-roll.
pub struct Scheduler {
    pre: Ms,
    post: Ms,
    full_pre: Ms,
    next_id: u64,
    pending: Vec<ClipJob>,
}

impl Scheduler {
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
    /// clip's end to `press + post_roll` and returns true; anything else starts a new clip.
    pub fn press(&mut self, p: Press) -> bool {
        if p.kind == Kind::Normal
            && let Some(job) = self
                .pending
                .iter_mut()
                .find(|j| j.kind == Kind::Normal && p.at < j.to)
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
