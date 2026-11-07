//! The clip player: ffmpeg decodes the clip at real-time pace (`-re`) from a start position,
//! scaled to the player's size, and its frames stream into the window as images. Seeking
//! restarts it at the new position. The player is muted; "Open" plays the file with sound.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use iced::futures::SinkExt as _;
use iced::widget::image::Handle;

/// Player frames are this wide (and as high as the clip's aspect asks).
pub const WIDTH: u32 = 960;
pub const FPS: u32 = 30;

#[derive(Debug, Clone)]
pub enum Event {
    Frame { at: f64, image: Handle },
    Ended,
}

/// The clip's size and duration, from ffprobe.
pub fn probe(ffmpeg: &Path, clip: &Path) -> Option<(u32, u32, f64)> {
    let ffprobe = ffmpeg.with_file_name("ffprobe.exe");
    let out = crate::platform::quiet_command(&ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height:format=duration",
            "-of",
            "default=nw=1",
        ])
        .arg(clip)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .and_then(|v| v.trim().parse::<f64>().ok())
    };
    Some((
        get("width")? as u32,
        get("height")? as u32,
        get("duration")?,
    ))
}

/// The player's frame height for a `w × h` clip.
pub fn height_for(w: u32, h: u32) -> u32 {
    ((u64::from(WIDTH) * u64::from(h) / u64::from(w.max(1))) as u32 / 2 * 2).max(2)
}

/// What one play run decodes; the subscription restarts when this changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Run {
    pub clip: PathBuf,
    /// Start position in milliseconds.
    pub from_ms: u64,
    pub width: u32,
    pub height: u32,
    /// Bumped on every Play/seek, so the same position can be played again.
    pub generation: u64,
}

/// The stream of frames of one run.
pub fn frames(ffmpeg: PathBuf, run: Run) -> impl iced::futures::Stream<Item = Event> {
    iced::stream::channel(4, move |mut out| async move {
        let (tx, mut rx) = iced::futures::channel::mpsc::channel::<Event>(2);
        let r = run.clone();
        std::thread::spawn(move || decode(&ffmpeg, &r, tx));
        use iced::futures::StreamExt as _;
        while let Some(e) = rx.next().await {
            if out.send(e).await.is_err() {
                break;
            }
        }
    })
}

fn decode(ffmpeg: &Path, r: &Run, mut tx: iced::futures::channel::mpsc::Sender<Event>) {
    let child = crate::platform::quiet_command(ffmpeg)
        .args(["-v", "error", "-nostdin", "-re", "-ss"])
        .arg(format!("{:.3}", r.from_ms as f64 / 1000.0))
        .arg("-i")
        .arg(&r.clip)
        .args(["-an", "-vf"])
        .arg(format!("fps={FPS},scale={}:{}", r.width, r.height))
        .args(["-pix_fmt", "rgba", "-f", "rawvideo", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        let _ = iced::futures::executor::block_on(tx.send(Event::Ended));
        return;
    };
    let Some(mut stdout) = child.stdout.take() else {
        return;
    };
    let size = (r.width * r.height * 4) as usize;
    let mut i = 0u64;
    loop {
        let mut buf = vec![0u8; size];
        if stdout.read_exact(&mut buf).is_err() {
            break;
        }
        let at = r.from_ms as f64 / 1000.0 + i as f64 / f64::from(FPS);
        i += 1;
        let e = Event::Frame {
            at,
            image: Handle::from_rgba(r.width, r.height, buf),
        };
        // The window stopped listening (paused, seeked, closed): stop decoding.
        if iced::futures::executor::block_on(tx.send(e)).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return;
        }
    }
    let _ = child.wait();
    let _ = iced::futures::executor::block_on(tx.send(Event::Ended));
}

/// One still frame at `at` seconds (shown while paused and after a seek).
pub fn still(ffmpeg: &Path, clip: &Path, at: f64, w: u32, h: u32) -> Option<Handle> {
    let out = crate::platform::quiet_command(ffmpeg)
        .args(["-v", "error", "-nostdin", "-ss"])
        .arg(format!("{at:.3}"))
        .arg("-i")
        .arg(clip)
        .args(["-frames:v", "1", "-an", "-vf"])
        .arg(format!("scale={w}:{h}"))
        .args(["-pix_fmt", "rgba", "-f", "rawvideo", "-"])
        .output()
        .ok()?;
    (out.stdout.len() == (w * h * 4) as usize).then(|| Handle::from_rgba(w, h, out.stdout))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Plays a real clip for about a second (needs the pinned ffmpeg and a test clip:
    /// `RETAKE_TEST_CLIP=<clip> cargo test -p retake-ui -- --ignored`).
    #[test]
    #[ignore]
    fn streams_frames_at_real_time() {
        use iced::futures::StreamExt as _;
        let clip = PathBuf::from(std::env::var("RETAKE_TEST_CLIP").expect("RETAKE_TEST_CLIP"));
        let ffmpeg =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\third_party\ffmpeg\bin\ffmpeg.exe");
        let (w, h, d) = probe(&ffmpeg, &clip).expect("probe");
        assert!(w > 0 && h > 0 && d > 1.0);
        let run = Run {
            clip,
            from_ms: 2_000,
            width: WIDTH,
            height: height_for(w, h),
            generation: 0,
        };
        let t = std::time::Instant::now();
        let n = iced::futures::executor::block_on(async {
            let mut s = Box::pin(frames(ffmpeg, run));
            let mut n = 0;
            let mut first_at = None;
            while let Some(e) = s.next().await {
                if let Event::Frame { at, .. } = e {
                    first_at.get_or_insert(at);
                    n += 1;
                    if n == 30 {
                        break;
                    }
                }
            }
            assert!((first_at.unwrap() - 2.0).abs() < 1e-6);
            n
        });
        let secs = t.elapsed().as_secs_f64();
        assert_eq!(n, 30);
        // Real-time pace: 30 frames at 30 fps take about a second, not a burst.
        assert!(secs > 0.8 && secs < 3.0, "{secs:.2} s");
    }

    #[test]
    fn frame_height_keeps_the_aspect_and_is_even() {
        assert_eq!(height_for(2560, 1440), 540);
        assert_eq!(height_for(1080, 1920), 1706);
        assert_eq!(height_for(3440, 1440), 400);
    }
}
