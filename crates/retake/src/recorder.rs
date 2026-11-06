//! The recording loop: polls for the game, drives the session state machine, runs ffmpeg and
//! reports the tray status. Runs on its own thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use retake_core::Ms;
use retake_core::buffer::{clear_dir, low_space_warning};
use retake_core::config::Capture;
use retake_core::ffmpeg_args::capture_args;
use retake_core::progress::{SpeedEvent, SpeedMonitor, Stats};
use retake_core::watcher::{Command, TrayStatus, Watcher, game_running, tray_status};

use crate::child::ChildJob;
use crate::drive::free_bytes;
use crate::ffmpeg_proc::{Ffmpeg, GRACE};
use crate::procs::process_names;

const TICK: Duration = Duration::from_millis(250);
const POLL: Ms = 2_000;
const SUMMARY_EVERY: Ms = 60_000;

pub struct Setup {
    pub ffmpeg: PathBuf,
    pub capture: Capture,
    pub processes: Vec<String>,
    /// A startup warning (monitor refresh, HDR) keeps the icon yellow.
    pub warning: bool,
    pub job: Arc<ChildJob>,
    pub notify: Box<dyn Fn(TrayStatus) + Send>,
}

enum Event {
    Stats(Stats),
    Shutdown,
}

pub struct Recorder {
    tx: Sender<Event>,
    thread: JoinHandle<()>,
}

impl Recorder {
    pub fn start(setup: Setup) -> Result<Recorder> {
        let (tx, rx) = mpsc::channel();
        let stats_tx = tx.clone();
        let thread = std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || Loop::new(setup, stats_tx).run(rx))
            .context("spawning recorder thread")?;
        Ok(Recorder { tx, thread })
    }

    /// Stops ffmpeg gracefully and waits for the thread.
    pub fn shutdown(self) {
        let _ = self.tx.send(Event::Shutdown);
        if self.thread.join().is_err() {
            tracing::error!("recorder thread panicked");
        }
    }
}

struct Loop {
    s: Setup,
    stats_tx: Sender<Event>,
    args: Vec<String>,
    epoch: Instant,
    watcher: Watcher,
    speed: SpeedMonitor,
    ffmpeg: Option<Ffmpeg>,
    last_stats: Option<Stats>,
    shown: Option<TrayStatus>,
}

impl Loop {
    fn new(s: Setup, stats_tx: Sender<Event>) -> Loop {
        let args = capture_args(&s.capture);
        tracing::info!(args = %args.join(" "), "ffmpeg command");
        Loop {
            s,
            stats_tx,
            args,
            epoch: Instant::now(),
            watcher: Watcher::new(),
            speed: SpeedMonitor::new(),
            ffmpeg: None,
            last_stats: None,
            shown: None,
        }
    }

    fn now(&self) -> Ms {
        self.epoch.elapsed().as_millis() as Ms
    }

    fn run(mut self, rx: Receiver<Event>) {
        let mut next_poll: Ms = 0;
        let mut next_summary: Ms = SUMMARY_EVERY;
        self.publish();
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Event::Stats(st)) => self.on_stats(st),
                Ok(Event::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = self.now();
            if now >= next_poll {
                next_poll = now + POLL;
                match process_names() {
                    Ok(names) => {
                        let running =
                            game_running(names.iter().map(String::as_str), &self.s.processes);
                        let cmd = self.watcher.on_game(now, running);
                        self.apply(cmd, now);
                    }
                    Err(e) => tracing::warn!("process snapshot failed: {e:#}"),
                }
            }
            if let Some(status) = self.ffmpeg.as_mut().and_then(Ffmpeg::exited) {
                tracing::warn!(%status, "ffmpeg exited unexpectedly");
                self.ffmpeg = None;
                self.speed.reset();
                let cmd = self.watcher.on_ffmpeg_exit(now);
                self.apply(cmd, now);
            }
            let cmd = self.watcher.on_tick(now);
            self.apply(cmd, now);
            if now >= next_summary {
                next_summary = now + SUMMARY_EVERY;
                self.log_summary();
            }
            self.publish();
        }
        if let Some(f) = self.ffmpeg.take() {
            f.stop(GRACE);
        }
    }

    fn on_stats(&mut self, st: Stats) {
        self.last_stats = Some(st);
        match self.speed.observe(self.now(), st.speed) {
            Some(SpeedEvent::FallingBehind) => {
                tracing::warn!(speed = st.speed, fps = st.fps, "encoder falling behind")
            }
            Some(SpeedEvent::Recovered) => tracing::info!(speed = st.speed, "encoder caught up"),
            None => {}
        }
    }

    /// Carries out `cmd`; a failed spawn feeds back into the state machine as an exit.
    fn apply(&mut self, mut cmd: Option<Command>, now: Ms) {
        while let Some(c) = cmd.take() {
            match c {
                Command::StartSession => {
                    tracing::info!("game running; starting recording");
                    let dir = &self.s.capture.buffer_dir;
                    match clear_dir(dir) {
                        Ok(n) => tracing::info!(deleted = n, "buffer cleared"),
                        Err(e) => tracing::warn!("clearing {}: {e}", dir.display()),
                    }
                    match free_bytes(dir) {
                        Ok(free) => {
                            if let Some(w) = low_space_warning(free, dir) {
                                tracing::warn!("{w}");
                            }
                        }
                        Err(e) => tracing::warn!("{e:#}"),
                    }
                    cmd = self.spawn(now);
                }
                Command::Respawn => {
                    tracing::info!("restarting ffmpeg");
                    cmd = self.spawn(now);
                }
                Command::StopSession => {
                    tracing::info!("game closed; stopping recording");
                    if let Some(f) = self.ffmpeg.take() {
                        f.stop(GRACE);
                    }
                    self.speed.reset();
                }
                Command::GiveUp => tracing::error!(
                    "ffmpeg crashed too often (5 restarts in 10 minutes); \
                     not retrying until the game is next launched"
                ),
            }
        }
    }

    fn spawn(&mut self, now: Ms) -> Option<Command> {
        let tx = self.stats_tx.clone();
        let on_stats = move |st| {
            let _ = tx.send(Event::Stats(st));
        };
        match Ffmpeg::spawn(&self.s.job, &self.s.ffmpeg, &self.args, on_stats) {
            Ok(f) => {
                tracing::info!(pid = f.pid(), "ffmpeg recording");
                self.ffmpeg = Some(f);
                None
            }
            Err(e) => {
                tracing::error!("starting ffmpeg: {e:#}");
                self.watcher.on_ffmpeg_exit(now)
            }
        }
    }

    fn log_summary(&self) {
        if let (Some(f), Some(st)) = (&self.ffmpeg, self.last_stats) {
            let mb = f.working_set().map_or(0, |b| b / (1024 * 1024));
            tracing::info!(
                fps = st.fps,
                speed = st.speed,
                frame = st.frame,
                ffmpeg_mb = mb,
                "recording"
            );
        }
    }

    fn publish(&mut self) {
        let s = tray_status(self.watcher.phase(), self.s.warning || self.speed.behind());
        if self.shown != Some(s) {
            tracing::info!(phase = ?self.watcher.phase(), status = ?s, "status");
            (self.s.notify)(s);
            self.shown = Some(s);
        }
    }
}
