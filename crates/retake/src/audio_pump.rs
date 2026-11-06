//! Feeds ffmpeg's audio inputs: one thread per track places captured packets on a
//! [`Timeline`] by capture time and, every 10 ms, writes everything due into the track's pipe,
//! zeros included. So each pipe carries exactly 48 000 frames per second of wall time from the
//! run's origin, whether or not the game makes a sound.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use retake_audio::{Capture, Source, unix_us};
use retake_core::audio::{Gate, Timeline, frames_at};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, SetWaitableTimer,
    TIMER_ALL_ACCESS, WaitForSingleObject,
};

use crate::pipe::OutPipe;

/// Packets may arrive this late after their capture time and still land in place (100 ms).
const HOLD_FRAMES: u64 = 4_800;
/// A write ffmpeg doesn't take within this long is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_millis(200);
/// How long ffmpeg gets to open a pipe.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Track {
    /// For the log: "game", "mic", "comms".
    pub name: &'static str,
    pub pipe: OutPipe,
    /// `None` = no source (it failed to resolve); the track carries silence.
    pub source: Option<Source>,
    /// Mic noise gate threshold in dBFS; 0 = off.
    pub gate_db: f32,
}

/// The running track threads. Dropping it stops them and closes the pipes.
pub struct Pump {
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Pump {
    /// Starts one thread per track. Each waits for ffmpeg to open its pipe, then starts its
    /// capture and writes. `origin_us` is the run's origin (Unix µs), the same one the video
    /// timestamps count from.
    pub fn start(tracks: Vec<Track>, origin_us: i64) -> Result<Pump> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        for t in tracks {
            let stop = stop.clone();
            threads.push(
                std::thread::Builder::new()
                    .name(format!("pump-{}", t.name))
                    .spawn(move || run(t, origin_us, &stop))
                    .context("spawning audio pump thread")?,
            );
        }
        Ok(Pump { stop, threads })
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

struct Timer(HANDLE);
impl Drop for Timer {
    fn drop(&mut self) {
        // SAFETY: we own the handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// A periodic 10 ms high-resolution waitable timer.
fn tick_timer() -> Result<Timer> {
    // SAFETY: plain Win32 calls; the due time outlives the call.
    unsafe {
        let t = Timer(
            CreateWaitableTimerExW(
                None,
                None,
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
            .context("CreateWaitableTimerExW")?,
        );
        let due = -100_000i64; // 10 ms from now, relative
        SetWaitableTimer(t.0, &due, 10, None, None, false).context("SetWaitableTimer")?;
        Ok(t)
    }
}

fn run(t: Track, origin_us: i64, stop: &AtomicBool) {
    if let Err(e) = t.pipe.wait_connected(CONNECT_TIMEOUT) {
        tracing::error!(track = t.name, "{e:#}");
        return;
    }
    let timeline = Arc::new(Mutex::new(Timeline::new(HOLD_FRAMES)));
    let _capture = t.source.clone().and_then(|source| {
        let tl = timeline.clone();
        let on_data = move |at: i64, frames: &[i16]| {
            tl.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(frames_at(at - origin_us), frames);
        };
        match Capture::start(source.clone(), on_data) {
            Ok(c) => {
                tracing::info!(track = t.name, source = c.description(), "audio capture");
                Some(c)
            }
            Err(e) => {
                tracing::warn!(track = t.name, "{source:?}: {e:#}; this track stays silent");
                None
            }
        }
    });
    if t.source.is_none() {
        tracing::info!(track = t.name, "no source; this track stays silent");
    }
    let timer = match tick_timer() {
        Ok(timer) => timer,
        Err(e) => {
            tracing::error!(track = t.name, "{e:#}");
            return;
        }
    };
    let mut gate = Gate::new(t.gate_db);
    let mut dropped_writes = 0u64;
    let mut late_logged = 0u64;
    let mut bytes = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: valid timer handle; a timeout just loops.
        unsafe { WaitForSingleObject(timer.0, 100) };
        let now = frames_at(unix_us() - origin_us).max(0) as u64;
        let (mut due, late) = {
            let mut tl = timeline.lock().unwrap_or_else(|e| e.into_inner());
            (tl.take_due(now), tl.late_frames())
        };
        if late > late_logged + 4_800 {
            tracing::warn!(track = t.name, late_frames = late, "audio packets arrived too late");
            late_logged = late;
        }
        if due.is_empty() {
            continue;
        }
        gate.process(&mut due);
        bytes.clear();
        bytes.extend(due.iter().flat_map(|s| s.to_le_bytes()));
        match t.pipe.write(&bytes, WRITE_TIMEOUT) {
            Ok(true) => {}
            Ok(false) => {
                dropped_writes += 1;
                tracing::warn!(
                    track = t.name,
                    dropped_writes,
                    "ffmpeg didn't take audio within 200 ms; dropped"
                );
            }
            Err(e) => {
                // ffmpeg exited or closed its input; the recorder restarts the run.
                tracing::info!(track = t.name, "audio pipe closed: {e}");
                break;
            }
        }
    }
}
