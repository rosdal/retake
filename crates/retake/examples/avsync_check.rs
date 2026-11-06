//! Measures A/V sync in a clip recorded from the `avsync` stand-in game: finds each white flash
//! in the video and each click in an audio track, pairs them and prints the offsets.
//!
//! `avsync_check <clip> <crop w:h:x:y> [audio track, default 0] [ffmpeg path]`
//! Positive offset = the sound comes after the flash.

use std::process::{Command, Stdio};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        eprintln!("usage: avsync_check <clip> <crop w:h:x:y> [audio track] [ffmpeg]");
        std::process::exit(2);
    }
    let clip = &args[0];
    let crop = &args[1];
    let track: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let ffmpeg = args
        .get(3)
        .cloned()
        .unwrap_or_else(|| "third_party/ffmpeg/bin/ffmpeg.exe".into());
    let ffprobe = ffmpeg.replace("ffmpeg.exe", "ffprobe.exe");

    // Stream start times, so both sides use the file's own timeline.
    let probe = Command::new(&ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_type,start_time",
            "-of",
            "csv=p=0",
            clip,
        ])
        .output()
        .expect("running ffprobe");
    let mut audio_starts = Vec::new();
    for line in String::from_utf8_lossy(&probe.stdout).lines() {
        let mut p = line.split(',');
        if let (Some("audio"), Some(t)) = (p.next(), p.next()) {
            audio_starts.push(t.trim().parse::<f64>().unwrap_or(0.0));
        }
    }
    let a_start = *audio_starts.get(track).expect("no such audio track");

    // Flashes: mean brightness of the window region per frame, with its pts.
    let v = Command::new(&ffmpeg)
        .args(["-v", "error", "-copyts", "-i", clip, "-map", "0:v:0", "-vf"])
        .arg(format!(
            "crop={crop},signalstats,metadata=print:key=lavfi.signalstats.YAVG:file=-"
        ))
        .args(["-f", "null", "-"])
        .output()
        .expect("running ffmpeg for video");
    let mut flashes = Vec::new();
    let (mut t, mut prev) = (0.0f64, 0.0f64);
    for line in String::from_utf8_lossy(&v.stdout).lines() {
        if let Some(i) = line.find("pts_time:") {
            t = line[i + 9..].trim().parse().unwrap_or(t);
        } else if let Some(y) = line.strip_prefix("lavfi.signalstats.YAVG=") {
            let y: f64 = y.trim().parse().unwrap_or(0.0);
            if y > 128.0 && prev < 64.0 {
                flashes.push(t);
            }
            prev = y;
        }
    }

    // Clicks: the first loud sample after at least 300 ms of quiet.
    let a = Command::new(&ffmpeg)
        .args(["-v", "error", "-copyts", "-i", clip, "-map"])
        .arg(format!("0:a:{track}"))
        .args(["-ac", "1", "-ar", "48000", "-f", "s16le", "-"])
        .stdout(Stdio::piped())
        .output()
        .expect("running ffmpeg for audio");
    let samples: Vec<i16> = a
        .stdout
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let mut clicks = Vec::new();
    let mut quiet = 0usize;
    for (i, &s) in samples.iter().enumerate() {
        if s.unsigned_abs() > 3_000 {
            if quiet >= 14_400 {
                clicks.push(a_start + i as f64 / 48_000.0);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }

    println!(
        "{} flashes, {} clicks (audio track {track} starts at {a_start:.3} s)",
        flashes.len(),
        clicks.len()
    );
    let mut offsets = Vec::new();
    for f in &flashes {
        if let Some(c) = clicks
            .iter()
            .min_by(|x, y| (*x - f).abs().total_cmp(&(*y - f).abs()))
            && (c - f).abs() < 0.3
        {
            let ms = (c - f) * 1000.0;
            println!("flash {f:9.3} s  click {c:9.3} s  offset {ms:+7.1} ms");
            offsets.push(ms);
        }
    }
    if offsets.is_empty() {
        println!("no pairs found");
        std::process::exit(1);
    }
    let mean = offsets.iter().sum::<f64>() / offsets.len() as f64;
    let worst = offsets.iter().fold(0.0f64, |m, o| m.max(o.abs()));
    println!(
        "pairs {}  mean {mean:+.1} ms  worst {worst:.1} ms",
        offsets.len()
    );
}
