//! The recording loop: polls for the game, drives the session state machine, runs ffmpeg and
//! reports the tray status. Runs on its own thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use retake_core::Ms;
use retake_core::buffer::clear_dir;
use retake_core::config::Capture;
use retake_core::ffmpeg_args::{capture_args, segment_count};
use retake_core::progress::{SpeedEvent, SpeedMonitor, Stats};
use retake_core::watcher::{Command, TrayStatus, Watcher, game_running, tray_status};

use crate::child::ChildJob;
use crate::clock::wall_ms;
use crate::drive::warn_if_low_space;
use crate::ffmpeg_proc::{Ffmpeg, GRACE};
use crate::procs::process_names;
use crate::saver::{Session, SessionSlot};

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
    /// The current ffmpeg run, for the saver.
    pub session: SessionSlot,
}

enum Event {
    /// A stats line from ffmpeg run number `.0`.
    Stats(u64, Stats),
    Shutdown,
}

/// The recorder thread. Dropping it stops ffmpeg gracefully and waits for the thread.
pub struct Recorder {
    tx: Sender<Event>,
    thread: Option<JoinHandle<()>>,
}

impl Recorder {
    pub fn start(setup: Setup) -> Result<Recorder> {
        let (tx, rx) = mpsc::channel();
        let stats_tx = tx.clone();
        let thread = std::thread::Builder::new()
            .name("recorder".into())
            .spawn(move || Loop::new(setup, stats_tx).run(rx))
            .context("spawning recorder thread")?;
        Ok(Recorder {
            tx,
            thread: Some(thread),
        })
    }

    /// Stops ffmpeg gracefully and waits for the thread (the same as dropping it).
    pub fn shutdown(self) {}
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.tx.send(Event::Shutdown);
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
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
    /// Counts ffmpeg starts; tags stats so a previous run's late lines are ignored.
    run: u64,
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
            run: 0,
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
                Ok(Event::Stats(run, st)) => self.on_stats(run, st),
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

    fn on_stats(&mut self, run: u64, st: Stats) {
        // A stopped or crashed ffmpeg's last lines can arrive after it is gone or replaced.
        if self.ffmpeg.is_none() || run != self.run {
            return;
        }
        self.last_stats = Some(st);
        match self.speed.observe(self.now(), st.time) {
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

    /// Empties the buffer and starts a new ffmpeg run. The buffer is emptied on a respawn too:
    /// the new run starts again at `seg_00` with timestamps from 0, so old segments left
    /// behind would no longer line up with `index.csv`.
    fn spawn(&mut self, now: Ms) -> Option<Command> {
        // Before the buffer is emptied, so a save in progress sees the run end.
        *self.s.session.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let dir = &self.s.capture.buffer_dir;
        match clear_dir(dir) {
            Ok(n) => tracing::info!(deleted = n, "buffer cleared"),
            Err(e) => tracing::warn!("clearing {}: {e}", dir.display()),
        }
        warn_if_low_space(dir);
        self.run += 1;
        self.speed.reset();
        self.last_stats = None;
        let tx = self.stats_tx.clone();
        let run = self.run;
        let on_stats = move |st| {
            let _ = tx.send(Event::Stats(run, st));
        };
        let start = wall_ms();
        match Ffmpeg::spawn(&self.s.job, &self.s.ffmpeg, &self.args, on_stats) {
            Ok(f) => {
                tracing::info!(pid = f.pid(), "ffmpeg recording");
                *self.s.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(Session {
                    run: self.run,
                    start,
                    buffer_dir: self.s.capture.buffer_dir.clone(),
                    wrap: segment_count(self.s.capture.buffer_secs),
                });
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
