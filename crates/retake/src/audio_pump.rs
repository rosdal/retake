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
/// Ticks (10 ms) between checks that the capture still runs on the right device (2 s).
const RECHECK_TICKS: u32 = 200;

/// Keeps a track's capture alive: restarts it when its thread has ended (device unplugged or
/// invalidated) or, for the default output or microphone, when Windows switches the default
/// device. Meanwhile the track carries silence.
struct SourceWatch {
    name: &'static str,
    source: Option<Source>,
    timeline: Arc<Mutex<Timeline>>,
    origin_us: i64,
    capture: Option<Capture>,
    /// For sources that follow the default device: its id when the capture started.
    device: Option<String>,
    /// The last state logged: true = capturing.
    up: Option<bool>,
}

impl SourceWatch {
    fn new(
        name: &'static str,
        source: Option<Source>,
        timeline: Arc<Mutex<Timeline>>,
        origin_us: i64,
    ) -> SourceWatch {
        if source.is_none() {
            tracing::info!(track = name, "no source; this track stays silent");
        }
        SourceWatch {
            name,
            source,
            timeline,
            origin_us,
            capture: None,
            device: None,
            up: None,
        }
    }

    /// Which default device this source follows: `Some(true)` output, `Some(false)` mic.
    fn follows_default(&self) -> Option<bool> {
        match &self.source {
            Some(Source::DeviceLoopback) => Some(true),
            Some(Source::Mic { name })
                if name.is_empty() || name.eq_ignore_ascii_case("default") =>
            {
                Some(false)
            }
            _ => None,
        }
    }

    fn check(&mut self, warning: &AtomicBool) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let now_device = self
            .follows_default()
            .and_then(retake_audio::default_device_id);
        let switched =
            self.capture.is_some() && self.follows_default().is_some() && now_device != self.device;
        let dead = self.capture.as_ref().is_none_or(Capture::finished);
        if !dead && !switched {
            return;
        }
        if switched {
            tracing::info!(track = self.name, "default device changed; following it");
        }
        self.capture = None;
        let tl = self.timeline.clone();
        let origin = self.origin_us;
        let on_data = move |at: i64, frames: &[i16]| {
            tl.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(frames_at(at - origin), frames);
        };
        match Capture::start(source.clone(), on_data) {
            Ok(c) => {
                tracing::info!(track = self.name, source = c.description(), "audio capture");
                self.capture = Some(c);
                self.device = now_device;
                if self.up == Some(false) {
                    tracing::info!(track = self.name, "audio source is back");
                }
                self.up = Some(true);
                warning.store(false, Ordering::Relaxed);
            }
            Err(e) => {
                if self.up != Some(false) {
                    tracing::warn!(
                        track = self.name,
                        "{source:?}: {e:#}; the track carries silence and Retake retries every 2 s"
                    );
                }
                self.up = Some(false);
                warning.store(true, Ordering::Relaxed);
            }
        }
    }
}

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
    /// One flag per track: its source is missing.
    warnings: Vec<Arc<AtomicBool>>,
}

impl Pump {
    /// Starts one thread per track. Each waits for ffmpeg to open its pipe, then starts its
    /// capture and writes. `origin_us` is the run's origin (Unix µs), the same one the video
    /// timestamps count from.
    pub fn start(tracks: Vec<Track>, origin_us: i64) -> Result<Pump> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        let mut warnings = Vec::new();
        for t in tracks {
            let stop = stop.clone();
            let warning = Arc::new(AtomicBool::new(false));
            warnings.push(warning.clone());
            threads.push(
                std::thread::Builder::new()
                    .name(format!("pump-{}", t.name))
                    .spawn(move || run(t, origin_us, &stop, &warning))
                    .context("spawning audio pump thread")?,
            );
        }
        Ok(Pump {
            stop,
            threads,
            warnings,
        })
    }

    /// Whether any track has lost its source (mic unplugged, output gone).
    pub fn warning(&self) -> bool {
        self.warnings.iter().any(|w| w.load(Ordering::Relaxed))
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

fn run(t: Track, origin_us: i64, stop: &AtomicBool, warning: &AtomicBool) {
    if let Err(e) = t.pipe.wait_connected(CONNECT_TIMEOUT) {
        tracing::error!(track = t.name, "{e:#}");
        return;
    }
    let timeline = Arc::new(Mutex::new(Timeline::new(HOLD_FRAMES)));
    let mut source = SourceWatch::new(t.name, t.source.clone(), timeline.clone(), origin_us);
    source.check(warning);
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
    let mut ticks = 0u32;
    while !stop.load(Ordering::Relaxed) {
        // SAFETY: valid timer handle; a timeout just loops.
        unsafe { WaitForSingleObject(timer.0, 100) };
        ticks += 1;
        if ticks.is_multiple_of(RECHECK_TICKS) {
            source.check(warning);
        }
        let now = frames_at(unix_us() - origin_us).max(0) as u64;
        let (mut due, late) = {
            let mut tl = timeline.lock().unwrap_or_else(|e| e.into_inner());
            (tl.take_due(now), tl.late_frames())
        };
        if late > late_logged + 4_800 {
            tracing::warn!(
                track = t.name,
                late_frames = late,
                "audio packets arrived too late"
            );
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
