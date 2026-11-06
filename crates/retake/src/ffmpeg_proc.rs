//! A running ffmpeg: spawned through the job, its stats parsed, stopped gracefully.

use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::process::{Child, ChildStdin, ExitStatus};
use std::time::{Duration, Instant};

use anyhow::Result;
use retake_core::progress::{Stats, parse_stats};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};

use crate::child::ChildJob;

/// How long ffmpeg gets to finish after `q` before it is terminated.
pub const GRACE: Duration = Duration::from_secs(3);

pub struct Ffmpeg {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Ffmpeg {
    /// Starts `exe args…`. Stats lines go to `on_stats`; every other stderr line is logged at
    /// `warn` (ffmpeg runs with `-loglevel warning`, so they are all warnings or errors).
    pub fn spawn(
        job: &ChildJob,
        exe: &Path,
        args: &[String],
        on_stats: impl Fn(Stats) + Send + 'static,
    ) -> Result<Ffmpeg> {
        let mut child = job.spawn_supervised(exe, args, move |line| match parse_stats(line) {
            Some(s) => on_stats(s),
            None => tracing::warn!(target: "ffmpeg", "{line}"),
        })?;
        let stdin = child.stdin.take();
        Ok(Ffmpeg { child, stdin })
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The exit status, if it has exited.
    pub fn exited(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Writes `q`, waits up to `grace` for a clean exit, then terminates it. Returns whether
    /// it exited by itself (and so finished the segment it was writing).
    pub fn stop(mut self, grace: Duration) -> bool {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(b"q").and_then(|()| stdin.flush());
        }
        let start = Instant::now();
        while start.elapsed() < grace {
            if let Some(status) = self.exited() {
                tracing::info!(pid = self.pid(), %status, "ffmpeg stopped");
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        tracing::warn!(
            pid = self.pid(),
            "ffmpeg ignored q for {grace:?}; terminating"
        );
        let _ = self.child.kill();
        let _ = self.child.wait();
        false
    }

    /// Current working set in bytes.
    pub fn working_set(&self) -> Option<u64> {
        let mut c = PROCESS_MEMORY_COUNTERS {
            cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            ..Default::default()
        };
        // SAFETY: our own child's handle, valid while `self.child` lives; `c` is sized correctly.
        unsafe { GetProcessMemoryInfo(HANDLE(self.child.as_raw_handle()), &mut c, c.cb) }.ok()?;
        Some(c.WorkingSetSize as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    fn cmd() -> PathBuf {
        PathBuf::from(std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into()))
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn stop_kills_a_child_that_ignores_q() {
        let job = ChildJob::new().unwrap();
        // ping ignores stdin, so the grace period runs out and it is terminated.
        let f = Ffmpeg::spawn(
            &job,
            Path::new("ping.exe"),
            &args(&["-n", "30", "127.0.0.1"]),
            |_| {},
        )
        .unwrap();
        let start = Instant::now();
        f.stop(Duration::from_millis(300));
        let took = start.elapsed();
        assert!(
            took >= Duration::from_millis(300) && took < Duration::from_secs(3),
            "{took:?}"
        );
    }

    #[test]
    fn exited_reports_a_finished_child() {
        let job = ChildJob::new().unwrap();
        let mut f = Ffmpeg::spawn(&job, &cmd(), &args(&["/C", "exit 4"]), |_| {}).unwrap();
        let start = Instant::now();
        let status = loop {
            if let Some(s) = f.exited() {
                break s;
            }
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.code(), Some(4));
    }

    #[test]
    fn stats_lines_reach_the_callback() {
        let job = ChildJob::new().unwrap();
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink = got.clone();
        let line = "frame=  5 fps= 60 q=1.0 size=N/A time=00:00:00.08 bitrate=N/A speed=1.00x";
        let mut f = Ffmpeg::spawn(
            &job,
            &cmd(),
            &[String::from("/C"), format!("echo {line} 1>&2")],
            move |s| sink.lock().unwrap().push(s),
        )
        .unwrap();
        while f.exited().is_none() {
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200)); // let the reader thread drain
        let got = got.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].frame, 5);
    }

    #[test]
    fn working_set_of_a_live_child() {
        let job = ChildJob::new().unwrap();
        let f = Ffmpeg::spawn(
            &job,
            Path::new("ping.exe"),
            &args(&["-n", "5", "127.0.0.1"]),
            |_| {},
        )
        .unwrap();
        assert!(f.working_set().unwrap() > 0);
        f.stop(Duration::from_millis(10));
    }

    /// The real segmenter with the pinned ffmpeg, a synthetic source and libx264 standing in
    /// for ddagrab + AMF: segments wrap, the index stays bounded, `q` stops it cleanly.
    /// Needs `scripts/fetch-ffmpeg.ps1`: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn pinned_ffmpeg_ring_buffer_wraps_and_stops_on_q() {
        use retake_core::ffmpeg_args::{INDEX_FILE, segment_args, segment_count};
        let exe =
            Path::new(env!("CARGO_MANIFEST_DIR")).join(r"..\..\third_party\ffmpeg\bin\ffmpeg.exe");
        let t = tempfile::tempdir().unwrap();
        // 6 s = 3 segments; only the segment arguments are used.
        let cap = retake_core::ffmpeg_args::CaptureSetup {
            output: 0,
            fps: 60,
            encoder: retake_core::autoconf::Encoder::Amf,
            codec: retake_core::config::Codec::Hevc,
            qp_i: 16,
            qp_p: 18,
            buffer_dir: t.path().to_path_buf(),
            buffer_secs: 6,
        };
        let mut a = args(&[
            "-hide_banner",
            "-loglevel",
            "warning",
            "-stats",
            "-stats_period",
            "1",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc=size=320x240:rate=60",
            "-c:v",
            "libx264",
            "-g",
            "120",
            "-bf",
            "0",
        ]);
        a.extend(segment_args(&cap));
        let stats = Arc::new(Mutex::new(0));
        let n = stats.clone();
        let job = ChildJob::new().unwrap();
        let mut f = Ffmpeg::spawn(&job, &exe, &a, move |_| *n.lock().unwrap() += 1).unwrap();
        std::thread::sleep(Duration::from_secs(9));
        assert!(f.exited().is_none(), "ffmpeg died early");
        let start = Instant::now();
        f.stop(GRACE);
        assert!(start.elapsed() < GRACE, "did not stop on q");
        let mut segs: Vec<_> = std::fs::read_dir(t.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".ts"))
            .collect();
        segs.sort();
        assert_eq!(segs, ["seg_00.ts", "seg_01.ts", "seg_02.ts"]);
        let index = std::fs::read_to_string(t.path().join(INDEX_FILE)).unwrap();
        assert_eq!(
            index.lines().count(),
            segment_count(cap.buffer_secs) as usize,
            "{index}"
        );
        assert!(*stats.lock().unwrap() >= 5, "stats lines not parsed");
    }
}
