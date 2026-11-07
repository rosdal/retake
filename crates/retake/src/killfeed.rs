//! Finding new kill-feed rows in Retake's own recordings (never live frames): ffmpeg decodes
//! the kill-feed region at 4 fps in greyscale and `retake_core::autoclip::new_rows` spots rows
//! that weren't there a frame earlier.

use std::path::Path;

use anyhow::{Context, Result};
use retake_core::autoclip::{FeedFrame, RowParams, feed_crop, new_rows};

use crate::child::ChildJob;
use crate::grader::probe;

/// Frames per second scanned.
pub const SCAN_FPS: u32 = 4;
/// The region is scaled to this height before scanning (rows are ~10 px).
const SCAN_HEIGHT: u32 = 120;

/// Times (seconds from the start of `video`) at which a new kill-feed row appeared.
pub fn scan(ffmpeg: &Path, job: &ChildJob, video: &Path) -> Result<Vec<f64>> {
    let input = vec!["-i".to_owned(), video.to_string_lossy().into_owned()];
    scan_input(ffmpeg, job, video, input)
}

/// Like [`scan`], over the segments named in a concat list (one pass over a whole session).
pub fn scan_list(ffmpeg: &Path, job: &ChildJob, list: &Path) -> Result<Vec<f64>> {
    let text = std::fs::read_to_string(list)?;
    let first = retake_core::clip::parse_concat_list(&text)
        .into_iter()
        .next()
        .context("empty segment list")?;
    let first = list.parent().unwrap_or(Path::new(".")).join(first);
    let input = ["-f", "concat", "-safe", "0", "-i"]
        .map(String::from)
        .into_iter()
        .chain([list.to_string_lossy().into_owned()])
        .collect();
    scan_input(ffmpeg, job, &first, input)
}

fn scan_input(
    ffmpeg: &Path,
    job: &ChildJob,
    sample: &Path,
    input: Vec<String>,
) -> Result<Vec<f64>> {
    let p = probe(ffmpeg, sample)?;
    let (cw, ch, cx, cy) = feed_crop(p.width, p.height);
    let sh = SCAN_HEIGHT;
    let sw = ((u64::from(cw) * u64::from(sh) / u64::from(ch.max(1))) as u32 / 2 * 2).max(2);
    let args: Vec<String> = vec![
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-hwaccel".into(),
        "d3d11va".into(),
    ];
    let mut args = args;
    args.extend(input);
    args.extend([
        "-map".into(),
        "0:v:0".into(),
        "-vf".into(),
        format!("fps={SCAN_FPS},crop={cw}:{ch}:{cx}:{cy},scale={sw}:{sh},format=gray"),
        "-f".into(),
        "rawvideo".into(),
        "-".into(),
    ]);
    let out = job.run_output(ffmpeg, &args)?;
    if !out.status.success() {
        anyhow::bail!("scanning the kill feed: ffmpeg exited with {}", out.status);
    }
    let size = (sw * sh) as usize;
    let frames: Vec<FeedFrame<'_>> = out
        .stdout
        .chunks_exact(size)
        .enumerate()
        .map(|(i, luma)| FeedFrame {
            time: i as f64 / f64::from(SCAN_FPS),
            width: sw as usize,
            height: sh as usize,
            luma,
        })
        .collect();
    Ok(new_rows(&frames, &RowParams::default()))
}

/// `retake.exe scan-feed <clip>`: prints when new kill-feed rows appear, for tuning.
pub fn scan_main(ffmpeg: &Path, words: &[String]) -> Result<()> {
    let clip = words.first().context("usage: retake scan-feed <clip>")?;
    let job = ChildJob::new()?;
    let rows = scan(ffmpeg, &job, Path::new(clip))?;
    println!("{} new kill-feed rows", rows.len());
    for t in rows {
        println!("  {t:7.2} s");
    }
    Ok(())
}
