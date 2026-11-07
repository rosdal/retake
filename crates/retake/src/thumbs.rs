//! Thumbnails for the window's library: `clips/thumbs/<stem>.jpg` (a frame at the first kill)
//! and `<stem>_strip.jpg` (8 frames around it, side by side, for the hover preview). Made by
//! the background queue, which waits until no game runs.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::background::Submitter;
use crate::child::ChildJob;
use crate::grader::probe;

/// Width of the still thumbnail and of each strip frame.
const THUMB_W: u32 = 320;
const STRIP_W: u32 = 240;
const STRIP_FRAMES: u32 = 8;
/// Seconds of clip the strip covers, centred on the kill.
const STRIP_SECS: f64 = 4.0;

pub fn dir(clips: &Path) -> PathBuf {
    clips.join("thumbs")
}

/// The thumbnail paths for a clip.
pub fn paths(clips: &Path, clip: &Path) -> (PathBuf, PathBuf) {
    let stem = clip.file_stem().unwrap_or_default().to_string_lossy();
    let d = dir(clips);
    (
        d.join(format!("{stem}.jpg")),
        d.join(format!("{stem}_strip.jpg")),
    )
}

/// Where the thumbnail is taken: the first kill (sidecar), else 70% in.
fn moment(clip: &Path, duration: f64) -> f64 {
    let first_kill = std::fs::read_to_string(clip.with_extension("json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.pointer("/autoclip/kills/0/at")
                .and_then(serde_json::Value::as_f64)
        });
    first_kill
        .unwrap_or(duration * 0.7)
        .clamp(0.0, (duration - 0.1).max(0.0))
}

/// Makes both thumbnails for `clip` (skips ones that exist).
pub fn make(ffmpeg: &Path, clips: &Path, clip: &Path) -> Result<()> {
    let (still, strip) = paths(clips, clip);
    if still.is_file() && strip.is_file() {
        return Ok(());
    }
    std::fs::create_dir_all(dir(clips))?;
    let p = probe(ffmpeg, clip)?;
    let at = moment(clip, p.duration);
    let job = ChildJob::new()?;
    let run = |args: Vec<String>| -> Result<()> {
        let o = job.run_output(ffmpeg, &args)?;
        if !o.status.success() {
            bail!("ffmpeg exited with {}", o.status);
        }
        Ok(())
    };
    let input = clip.to_string_lossy().into_owned();
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    if !still.is_file() {
        let mut a = s(&["-v", "error", "-nostdin", "-y", "-ss"]);
        a.push(format!("{at:.2}"));
        a.extend(s(&["-i", &input, "-frames:v", "1", "-vf"]));
        a.push(format!("scale={THUMB_W}:-2"));
        a.extend(s(&["-q:v", "4"]));
        a.push(still.to_string_lossy().into_owned());
        run(a)?;
    }
    if !strip.is_file() {
        let start = (at - STRIP_SECS / 2.0).max(0.0);
        let fps = f64::from(STRIP_FRAMES) / STRIP_SECS;
        let mut a = s(&["-v", "error", "-nostdin", "-y", "-ss"]);
        a.push(format!("{start:.2}"));
        a.extend(s(&["-t"]));
        a.push(format!("{STRIP_SECS}"));
        a.extend(s(&["-i", &input, "-frames:v", "1", "-vf"]));
        a.push(format!(
            "fps={fps},scale={STRIP_W}:-2,tile={STRIP_FRAMES}x1"
        ));
        a.extend(s(&["-q:v", "5"]));
        a.push(strip.to_string_lossy().into_owned());
        run(a)?;
    }
    Ok(())
}

/// Queues thumbnails for `clip` on the background queue.
pub fn queue(jobs: &Submitter, ffmpeg: &Path, clips: &Path, clip: &Path) {
    let (ffmpeg, clips, clip) = (
        ffmpeg.to_path_buf(),
        clips.to_path_buf(),
        clip.to_path_buf(),
    );
    jobs.submit("thumbnails", move || {
        if let Err(e) = make(&ffmpeg, &clips, &clip) {
            tracing::warn!("thumbnails for {}: {e:#}", clip.display());
        }
    });
}

/// Queues thumbnails for every raw clip that has none yet (at start, and after a rescan).
pub fn queue_missing(jobs: &Submitter, ffmpeg: &Path, clips: &Path) {
    let Ok(rd) = std::fs::read_dir(clips.join("raw")) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = p
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if p.extension().is_some_and(|x| x == "mkv") && !name.ends_with("_graded") {
            let (still, strip) = paths(clips, &p);
            if !still.is_file() || !strip.is_file() {
                queue(jobs, ffmpeg, clips, &p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbs_sit_in_the_thumbs_folder_by_stem() {
        let (a, b) = paths(Path::new(r"D:\clips"), Path::new(r"D:\clips\raw\x_3K.mkv"));
        assert_eq!(a, PathBuf::from(r"D:\clips\thumbs\x_3K.jpg"));
        assert_eq!(b, PathBuf::from(r"D:\clips\thumbs\x_3K_strip.jpg"));
    }

    #[test]
    fn the_moment_is_the_first_kill_or_late_in_the_clip() {
        let d = tempfile::tempdir().unwrap();
        let c = d.path().join("a.mkv");
        assert!((moment(&c, 20.0) - 14.0).abs() < 1e-9);
        std::fs::write(
            c.with_extension("json"),
            r#"{"autoclip":{"kills":[{"at":15.5}]}}"#,
        )
        .unwrap();
        assert!((moment(&c, 20.0) - 15.5).abs() < 1e-9);
        assert!(
            (moment(&c, 10.0) - 9.9).abs() < 1e-9,
            "clamped inside the clip"
        );
    }
}
