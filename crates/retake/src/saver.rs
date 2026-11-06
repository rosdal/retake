// Used from the saver thread and wiring added next; removed once they land.
#![allow(dead_code)]
//! Saving clips: copy the chosen segments out of the ring, check none was overwritten while
//! copying, join them with ffmpeg (stream copy) and publish the file under its final name.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use retake_core::Ms;
use retake_core::clip::{ClipJob, Kind, clip_name, concat_args, concat_list, numbered};
use retake_core::ffmpeg_args::INDEX_FILE;
use retake_core::segments::{Segment, parse_index, select, still_intact, writing_file};

use crate::child::ChildJob;

/// The ffmpeg run whose segments are in the buffer now.
#[derive(Debug, Clone)]
pub struct Session {
    /// The recorder's run counter; changes whenever ffmpeg is restarted and the buffer emptied.
    pub run: u64,
    /// Wall-clock ms when this ffmpeg run was spawned: `index.csv` time 0.
    pub start: Ms,
    pub buffer_dir: PathBuf,
    /// Number of ring slots.
    pub wrap: u32,
}

/// Written by the recorder, read by the saver. `None` while no run's segments are usable.
pub type SessionSlot = Arc<Mutex<Option<Session>>>;

pub fn current(slot: &SessionSlot) -> Option<Session> {
    slot.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// `raw/` holds finished clips; `tmp/` holds one work folder per running save.
#[derive(Debug, Clone)]
pub struct ClipDirs {
    pub raw: PathBuf,
    pub tmp: PathBuf,
}

impl ClipDirs {
    pub fn new(out_dir: &Path) -> ClipDirs {
        ClipDirs {
            raw: out_dir.join("raw"),
            tmp: out_dir.join("tmp"),
        }
    }

    /// Creates both folders and deletes whatever an earlier run left in `tmp/`.
    pub fn prepare(&self) -> io::Result<()> {
        fs::create_dir_all(&self.raw)?;
        if self.tmp.exists() {
            fs::remove_dir_all(&self.tmp)?;
        }
        fs::create_dir_all(&self.tmp)
    }
}

/// Reads and parses `index.csv`, retrying briefly when it is caught mid-rewrite.
pub fn read_index(dir: &Path, start: Ms) -> Vec<Segment> {
    for _ in 0..5 {
        if let Ok(text) = fs::read_to_string(dir.join(INDEX_FILE)) {
            let segs = parse_index(&text, start);
            if !segs.is_empty() {
                return segs;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Vec::new()
}

/// Copies with `File::open`, which shares read, write and delete access, so ffmpeg can keep
/// rewriting the ring and replacing the index while we read. Don't use `fs::copy` here.
fn copy_shared(src: &Path, dst: &Path) -> io::Result<u64> {
    let mut from = File::open(src)?;
    let mut to = File::create(dst)?;
    io::copy(&mut from, &mut to)
}

/// Removes its folder when dropped, however the save ends.
struct WorkDir(PathBuf);

impl Drop for WorkDir {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.0) {
            tracing::warn!(dir = %self.0.display(), "removing clip work folder: {e}");
        }
    }
}

/// Writes `job` as an `.mkv` in `dirs.raw` and returns its path. `concat(list, out)` joins the
/// listed files into `out` (ffmpeg in the app, a stand-in in tests).
pub fn save(
    job: &ClipJob,
    slot: &SessionSlot,
    dirs: &ClipDirs,
    concat: &dyn Fn(&Path, &Path) -> Result<()>,
) -> Result<PathBuf> {
    let s = current(slot).context("not recording; no clip saved")?;
    let listed = read_index(&s.buffer_dir, s.start);
    let writing = writing_file(&listed, s.wrap);
    let picked = select(&listed, job.press, job.from, job.to, writing.as_deref());
    if picked.is_empty() {
        bail!("no buffered video covers the press");
    }

    let work = WorkDir(dirs.tmp.join(job.id.to_string()));
    fs::create_dir_all(&work.0).with_context(|| format!("creating {}", work.0.display()))?;
    let names: Vec<String> = (0..picked.len()).map(|i| format!("{i:03}.ts")).collect();
    for (seg, name) in picked.iter().zip(&names) {
        copy_shared(&s.buffer_dir.join(&seg.file), &work.0.join(name))
            .with_context(|| format!("copying {}", seg.file))?;
    }

    if current(slot).map(|x| x.run) != Some(s.run) {
        bail!("recording restarted during the save");
    }
    let after = read_index(&s.buffer_dir, s.start);
    let ok = still_intact(&picked, &after, s.wrap);
    let intact: Vec<Segment> = picked
        .iter()
        .zip(&ok)
        .filter(|(_, ok)| **ok)
        .map(|(seg, _)| seg.clone())
        .collect();
    let keep = select(&intact, job.press, job.from, job.to, None);
    if keep.is_empty() {
        bail!("every segment was overwritten while copying");
    }
    if keep.len() < picked.len() {
        tracing::warn!(
            dropped = picked.len() - keep.len(),
            "segments overwritten while copying were left out"
        );
    }
    let kept_names: Vec<String> = keep
        .iter()
        .map(|k| {
            names[picked
                .iter()
                .position(|p| p == k)
                .expect("kept from picked")]
            .clone()
        })
        .collect();
    let list = work.0.join("list.txt");
    fs::write(&list, concat_list(&kept_names)).context("writing concat list")?;

    let name = clip_name(job.stamp, job.kind);
    let mut out = dirs.raw.join(&name);
    let mut n = 2;
    while out.exists() {
        out = dirs.raw.join(numbered(&name, n));
        n += 1;
    }
    let part = out.with_file_name(format!(
        "{}.part",
        out.file_name().expect("has a name").to_string_lossy()
    ));
    if let Err(e) = concat(&list, &part) {
        let _ = fs::remove_file(&part);
        return Err(e);
    }
    fs::rename(&part, &out).with_context(|| format!("renaming to {}", out.display()))?;
    tracing::info!(
        clip = %out.display(),
        segments = keep.len(),
        secs = (keep[keep.len() - 1].end - keep[0].start) / 1000,
        full = job.kind == Kind::Full,
        "clip saved"
    );
    Ok(out)
}

/// Runs the concat ffmpeg at below-normal priority and waits for it.
pub fn run_concat(ffmpeg: &Path, job: &ChildJob, list: &Path, out: &Path) -> Result<()> {
    let mut child = job.spawn_logged_low(ffmpeg, &concat_args(list, out))?;
    let status = child.wait().context("waiting for concat ffmpeg")?;
    if !status.success() {
        bail!("concat ffmpeg exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use retake_core::clip::Stamp;

    const START: Ms = 1_000_000_000;
    const T: Stamp = Stamp {
        year: 2026,
        month: 11,
        day: 6,
        hour: 20,
        minute: 15,
        second: 33,
    };

    /// A full 30-slot ring (60 s of segments, slots wrapping at seg_10) in `dir`; each segment's
    /// content is its start second as text.
    fn fake_ring(dir: &Path) {
        let mut index = String::new();
        for i in 0..30u32 {
            let file = format!("seg_{:02}.ts", (10 + i) % 30);
            let s = 2 * i;
            fs::write(dir.join(&file), format!("[{s}]")).unwrap();
            index.push_str(&format!("{file},{s}.000000,{}.000000\n", s + 2));
        }
        fs::write(dir.join(INDEX_FILE), index).unwrap();
    }

    fn setup() -> (tempfile::TempDir, SessionSlot, ClipDirs) {
        let t = tempfile::tempdir().unwrap();
        let buf = t.path().join("buffer");
        fs::create_dir(&buf).unwrap();
        fake_ring(&buf);
        let slot: SessionSlot = Arc::new(Mutex::new(Some(Session {
            run: 1,
            start: START,
            buffer_dir: buf,
            wrap: 30,
        })));
        let dirs = ClipDirs::new(&t.path().join("clips"));
        dirs.prepare().unwrap();
        (t, slot, dirs)
    }

    /// Stands in for ffmpeg: concatenates the listed files' bytes.
    fn cat(list: &Path, out: &Path) -> Result<()> {
        let dir = list.parent().unwrap();
        let mut bytes = Vec::new();
        for line in fs::read_to_string(list)?.lines() {
            let name = line.trim_start_matches("file '").trim_end_matches('\'');
            bytes.extend(fs::read(dir.join(name))?);
        }
        fs::write(out, bytes)?;
        Ok(())
    }

    fn job_at(kind: Kind, press_s: u64, from_s: u64, to_s: u64) -> ClipJob {
        let ms = |s: u64| START + s * 1000;
        ClipJob {
            id: 7,
            kind,
            press: ms(press_s),
            stamp: T,
            from: ms(from_s),
            to: ms(to_s),
        }
    }

    #[test]
    fn saves_the_window_in_order_and_cleans_up() {
        let (_t, slot, dirs) = setup();
        let out = save(&job_at(Kind::Normal, 40, 25, 43), &slot, &dirs, &cat).unwrap();
        assert_eq!(out, dirs.raw.join("2026-11-06_201533.mkv"));
        assert_eq!(
            fs::read_to_string(&out).unwrap(),
            "[24][26][28][30][32][34][36][38][40][42]"
        );
        assert_eq!(fs::read_dir(&dirs.tmp).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&dirs.raw).unwrap().count(), 1);
    }

    #[test]
    fn full_save_skips_the_segment_being_written() {
        let (_t, slot, dirs) = setup();
        let out = save(&job_at(Kind::Full, 59, 0, 62), &slot, &dirs, &cat).unwrap();
        assert!(out.ends_with("2026-11-06_201533_full.mkv"));
        let text = fs::read_to_string(&out).unwrap();
        assert!(
            text.starts_with("[2][4]") && text.ends_with("[58]"),
            "{text}"
        );
    }

    #[test]
    fn taken_name_gets_a_number() {
        let (_t, slot, dirs) = setup();
        fs::write(dirs.raw.join("2026-11-06_201533.mkv"), "older").unwrap();
        let out = save(&job_at(Kind::Normal, 40, 25, 43), &slot, &dirs, &cat).unwrap();
        assert!(out.ends_with("2026-11-06_201533_2.mkv"));
    }

    #[test]
    fn failed_concat_leaves_nothing_behind() {
        let (_t, slot, dirs) = setup();
        let fail = |_: &Path, out: &Path| -> Result<()> {
            fs::write(out, "half")?;
            anyhow::bail!("boom")
        };
        let e = save(&job_at(Kind::Normal, 40, 25, 43), &slot, &dirs, &fail).unwrap_err();
        assert!(format!("{e:#}").contains("boom"));
        assert_eq!(fs::read_dir(&dirs.raw).unwrap().count(), 0);
        assert_eq!(fs::read_dir(&dirs.tmp).unwrap().count(), 0);
    }

    #[test]
    fn no_session_or_no_video_is_an_error() {
        let (_t, slot, dirs) = setup();
        let late = job_at(Kind::Normal, 500, 485, 503);
        let e = save(&late, &slot, &dirs, &cat).unwrap_err().to_string();
        assert!(e.contains("no buffered video"), "{e}");
        *slot.lock().unwrap() = None;
        let e = save(&job_at(Kind::Normal, 40, 25, 43), &slot, &dirs, &cat)
            .unwrap_err()
            .to_string();
        assert!(e.contains("not recording"), "{e}");
    }

    #[test]
    fn prepare_empties_tmp_and_keeps_raw() {
        let t = tempfile::tempdir().unwrap();
        let dirs = ClipDirs::new(t.path());
        dirs.prepare().unwrap();
        fs::create_dir(dirs.tmp.join("3")).unwrap();
        fs::write(dirs.tmp.join("3").join("000.ts"), "x").unwrap();
        fs::write(dirs.raw.join("keep.mkv"), "x").unwrap();
        dirs.prepare().unwrap();
        assert_eq!(fs::read_dir(&dirs.tmp).unwrap().count(), 0);
        assert!(dirs.raw.join("keep.mkv").exists());
    }
}
