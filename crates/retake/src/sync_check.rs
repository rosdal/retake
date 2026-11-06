//! `retake.exe sync-check <clip> [--crop w:h:x:y] [--track N]`: measures A/V sync in a clip.
//! ffmpeg extracts the brightness of a region per frame and an audio track as PCM;
//! `retake_core::sync` pairs flashes with sounds.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use retake_core::sync::{SyncReport, onsets, pair, passes, report, rising_edges};

use crate::child::run_capture;

pub struct Args {
    pub clip: PathBuf,
    pub crop: Option<String>,
    pub track: usize,
}

/// Parses the words after `sync-check`.
pub fn parse(words: &[String]) -> Result<Args> {
    let mut clip = None;
    let mut crop = None;
    let mut track = 0;
    let mut it = words.iter();
    while let Some(w) = it.next() {
        match w.as_str() {
            "--crop" => crop = Some(it.next().context("--crop needs w:h:x:y")?.clone()),
            "--track" => {
                track = it
                    .next()
                    .context("--track needs a number")?
                    .parse()
                    .context("--track needs a number")?
            }
            _ if clip.is_none() => clip = Some(PathBuf::from(w)),
            _ => bail!("unexpected argument {w}"),
        }
    }
    Ok(Args {
        clip: clip.context("usage: retake sync-check <clip> [--crop w:h:x:y] [--track N]")?,
        crop,
        track,
    })
}

fn probe(ffprobe: &Path, clip: &Path, entries: &str, select: &str) -> Result<String> {
    run_capture(
        ffprobe,
        &[
            "-v",
            "error",
            "-select_streams",
            select,
            "-show_entries",
            entries,
            "-of",
            "csv=p=0",
            &clip.to_string_lossy(),
        ],
    )
}

/// Measures `args.clip` and prints each pair and the summary. Returns the report.
pub fn run(ffmpeg: &Path, args: &Args) -> Result<SyncReport> {
    let ffprobe = ffmpeg.with_file_name("ffprobe.exe");
    let clip = &args.clip;
    let crop = match &args.crop {
        Some(c) => c.clone(),
        None => {
            let wh = probe(&ffprobe, clip, "stream=width,height", "v:0")?;
            let mut p = wh.trim().split(',').map(|s| s.parse::<u32>().unwrap_or(0));
            let (w, h) = (p.next().unwrap_or(0), p.next().unwrap_or(0));
            if w == 0 || h == 0 {
                bail!("{} has no video size", clip.display());
            }
            // The centre fifth, where a crosshair's muzzle flash lands.
            format!("{}:{}:{}:{}", w / 5, h / 5, w * 2 / 5, h * 2 / 5)
        }
    };
    let a_start: f64 = probe(
        &ffprobe,
        clip,
        "stream=start_time",
        &format!("a:{}", args.track),
    )?
    .trim()
    .parse()
    .with_context(|| format!("{} has no audio track {}", clip.display(), args.track))?;

    let video = Command::new(ffmpeg)
        .args(["-v", "error", "-copyts", "-i"])
        .arg(clip)
        .args(["-map", "0:v:0", "-vf"])
        .arg(format!(
            "crop={crop},signalstats,metadata=print:key=lavfi.signalstats.YAVG:file=-"
        ))
        .args(["-f", "null", "-"])
        .output()
        .context("running ffmpeg for the video")?;
    let mut series = Vec::new();
    let mut t = 0.0;
    for line in String::from_utf8_lossy(&video.stdout).lines() {
        if let Some(i) = line.find("pts_time:") {
            t = line[i + 9..].trim().parse().unwrap_or(t);
        } else if let Some(y) = line.strip_prefix("lavfi.signalstats.YAVG=") {
            series.push((t, y.trim().parse().unwrap_or(0.0)));
        }
    }
    let audio = Command::new(ffmpeg)
        .args(["-v", "error", "-copyts", "-i"])
        .arg(clip)
        .arg("-map")
        .arg(format!("0:a:{}", args.track))
        .args(["-ac", "1", "-ar", "48000", "-f", "s16le", "-"])
        .output()
        .context("running ffmpeg for the audio")?;
    let samples: Vec<i16> = audio
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();

    let flashes = rising_edges(&series, 128.0, 64.0);
    let sounds = onsets(&samples, 48_000, a_start, 3_000, 0.3);
    let pairs = pair(&flashes, &sounds, 0.3);
    println!(
        "{}: {} flashes, {} sounds, crop {crop}, audio track {}",
        clip.display(),
        flashes.len(),
        sounds.len(),
        args.track
    );
    for (f, o) in &pairs {
        println!("  flash at {f:8.3} s  offset {o:+7.1} ms");
    }
    let offsets: Vec<f64> = pairs.iter().map(|p| p.1).collect();
    let Some(r) = report(&offsets) else {
        bail!("no flash had a sound within 300 ms");
    };
    println!(
        "pairs {}  median {:+.1} ms  worst {:.1} ms  {}",
        r.pairs,
        r.median_ms,
        r.worst_ms,
        if passes(&r) { "PASS" } else { "FAIL" }
    );
    Ok(r)
}
