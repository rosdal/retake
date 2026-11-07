//! The recording loop: polls for the game, drives the session state machine, runs ffmpeg and
//! reports the tray status. Runs on its own thread.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use retake_core::Ms;
use retake_core::autoconf::{Auto, Encoder, MonitorChoice, follow_game, pick_fps, pick_monitor};
use retake_core::buffer::clear_dir;
use retake_core::config::{Audio, Capture, GameSource};
use retake_core::disk::{DiskEvent, DiskGuard};
use retake_core::display::OutputInfo;
use retake_core::ffmpeg_args::{
    AUDIO_DIR, AudioInput, CaptureSetup, audio_capture_args, capture_args, segment_count,
};
use retake_core::progress::{SpeedEvent, SpeedMonitor, Stats};
use retake_core::watcher::{Command, Phase, TrayStatus, Watcher, game_running, tray_status};

use crate::audio_pump::{Pump, Track};
use crate::background::SharedGate;
use crate::child::ChildJob;
use crate::clock::now_ms;
use crate::drive::{free_bytes, warn_if_low_space};
use crate::ffmpeg_proc::{Ffmpeg, GRACE};
use crate::pipe::{OutPipe, pipe_name};
use crate::procs::processes;
use crate::saver::{Session, SessionSlot};
use crate::window::game_monitor;

const TICK: Duration = Duration::from_millis(250);
const POLL: Ms = 2_000;
const SUMMARY_EVERY: Ms = 60_000;
/// Polls in a row Siege's window must be on another monitor before the recording follows it.
const FOLLOW_POLLS: u32 = 3;

pub struct Setup {
    pub ffmpeg: PathBuf,
    /// The capture settings as configured; `"auto"` values are decided per session.
    pub capture: Capture,
    pub audio: Audio,
    /// `--diag`: log memory and handle counts every 5 minutes.
    pub diag: bool,
    pub encoder: Encoder,
    pub processes: Vec<String>,
    /// A startup warning (monitor refresh, HDR) keeps the icon yellow.
    pub warning: bool,
    pub job: Arc<ChildJob>,
    pub notify: Box<dyn Fn(TrayStatus) + Send>,
    /// The current ffmpeg run, for the saver.
    pub session: SessionSlot,
    /// Game-time quiet mode, updated on every poll.
    pub quiet: SharedGate,
    /// Called when a recording session ends (the game closed).
    pub on_session_end: Box<dyn Fn() + Send>,
}

enum Event {
    /// A stats line from ffmpeg run number `.0`.
    Stats(u64, Stats),
    /// The PC is going to sleep (`true`) or has resumed (`false`).
    Power(bool),
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

    /// The PC is going to sleep (`true`): stop recording now. Resumed (`false`): record again
    /// if the game still runs.
    pub fn power(&self, suspending: bool) {
        let _ = self.tx.send(Event::Power(suspending));
    }
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
    epoch: Instant,
    /// What the running ffmpeg was started with.
    current: Option<CaptureSetup>,
    /// Monitors as of the last start.
    outputs: Vec<OutputInfo>,
    /// Siege's process ids at the last poll.
    game_pids: Vec<u32>,
    /// The current session's monitor has a warning (refresh, HDR).
    session_warning: bool,
    /// The monitor Siege's window has moved to, and on how many polls in a row.
    move_to: Option<(u32, u32)>,
    watcher: Watcher,
    speed: SpeedMonitor,
    ffmpeg: Option<Ffmpeg>,
    /// The audio recorder (its own ffmpeg, see `audio_capture_args`) and the pump feeding it.
    audio_ffmpeg: Option<Ffmpeg>,
    pump: Option<Pump>,
    /// The game process the game track captures, to notice a restarted game.
    audio_pid: Option<u32>,
    /// Counts game sessions; a respawn within one keeps the number.
    game_session: u64,
    /// Free space on the buffer drive; recording stops while it is low.
    disk: DiskGuard,
    /// The PC is going to sleep: recording stops until it resumes.
    suspended: bool,
    /// Counts ffmpeg starts; tags stats so a previous run's late lines are ignored.
    run: u64,
    last_stats: Option<Stats>,
    shown: Option<TrayStatus>,
}

impl Loop {
    fn new(s: Setup, stats_tx: Sender<Event>) -> Loop {
        Loop {
            s,
            stats_tx,
            epoch: Instant::now(),
            current: None,
            outputs: Vec::new(),
            game_pids: Vec::new(),
            session_warning: false,
            move_to: None,
            watcher: Watcher::new(),
            speed: SpeedMonitor::new(),
            ffmpeg: None,
            audio_ffmpeg: None,
            pump: None,
            audio_pid: None,
            disk: DiskGuard::new(),
            game_session: 0,
            suspended: false,
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
        let mut next_diag: Ms = 0;
        self.publish();
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Event::Stats(run, st)) => self.on_stats(run, st),
                Ok(Event::Power(suspending)) => {
                    self.suspended = suspending;
                    if suspending {
                        tracing::info!("PC going to sleep; stopping the recording");
                        let cmd = self.watcher.stop_now(self.now());
                        self.apply(cmd, self.now());
                    } else {
                        tracing::info!("PC resumed; recording again if the game still runs");
                    }
                }
                Ok(Event::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = self.now();
            if now >= next_poll {
                next_poll = now + POLL;
                match processes() {
                    Ok(procs) => {
                        self.game_pids = procs
                            .iter()
                            .filter(|(_, n)| game_running([n.as_str()], &self.s.processes))
                            .map(|(p, _)| *p)
                            .collect();
                        let running = !self.game_pids.is_empty();
                        self.check_disk();
                        // Low disk or sleep: the watcher sees "no game" and stops the session
                        // (and won't start one) through its normal path.
                        let may_record = running && !self.disk.low() && !self.suspended;
                        let changed = self
                            .s
                            .quiet
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .on_game(running, now_ms());
                        match changed {
                            Some(true) => tracing::info!("quiet mode on: game running"),
                            Some(false) => {
                                tracing::info!("game closed; background work may start in 30 s")
                            }
                            None => {}
                        }
                        let cmd = self.watcher.on_game(now, may_record);
                        self.apply(cmd, now);
                        self.follow_game_window(now);
                        self.follow_game_pid(now);
                    }
                    Err(e) => tracing::warn!("process snapshot failed: {e:#}"),
                }
            }
            if let Some(status) = self.ffmpeg.as_mut().and_then(Ffmpeg::exited) {
                tracing::warn!(%status, "ffmpeg exited unexpectedly");
                self.ffmpeg = None;
                self.stop_ffmpeg();
                self.speed.reset();
                let cmd = self.watcher.on_ffmpeg_exit(now);
                self.apply(cmd, now);
            }
            if let Some(status) = self.audio_ffmpeg.as_mut().and_then(Ffmpeg::exited) {
                // The run restarts as a whole, so video and audio keep one origin.
                tracing::warn!(%status, "audio ffmpeg exited unexpectedly; restarting the recording");
                self.audio_ffmpeg = None;
                self.stop_ffmpeg();
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
            if self.s.diag && now >= next_diag {
                next_diag = now + crate::diag::EVERY_MS;
                self.log_diag();
            }
            self.publish();
        }
        self.stop_ffmpeg();
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
                    self.game_session += 1;
                    cmd = self.spawn(now);
                }
                Command::Respawn => {
                    tracing::info!("restarting ffmpeg");
                    cmd = self.spawn(now);
                }
                Command::StopSession => {
                    tracing::info!("stopping recording");
                    // No ffmpeg here means it crashed earlier, mid-segment.
                    let clean = self.stop_ffmpeg();
                    self.speed.reset();
                    // The segments stay saveable, but nothing is written any more.
                    if let Some(s) = self
                        .s
                        .session
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_mut()
                    {
                        s.live = false;
                        s.torn = !clean;
                    }
                    (self.s.on_session_end)();
                }
                Command::GiveUp => tracing::error!(
                    "ffmpeg crashed too often (5 restarts in 10 minutes); \
                     not retrying until the game is next launched"
                ),
            }
        }
    }

    /// With `output = "auto"`, restarts the recording on the monitor Siege's window is on once
    /// that window has stayed somewhere other than where ffmpeg is recording for
    /// [`FOLLOW_POLLS`] polls in a row. A restart empties the buffer, so a window that
    /// flickers between monitors must not trigger one each time.
    fn follow_game_window(&mut self, now: Ms) {
        if self.s.capture.output != Auto::Auto
            || self.ffmpeg.is_none()
            || self.watcher.phase() != Phase::Recording
        {
            self.move_to = None;
            return;
        }
        let Some(current) = self.current.as_ref().map(|c| c.output) else {
            return;
        };
        let gm = game_monitor(&self.game_pids);
        let target = follow_game(current, self.s.capture.output, &self.outputs, gm);
        self.move_to = match (target, self.move_to) {
            (Some(to), Some((seen, n))) if seen == to => Some((to, n + 1)),
            (Some(to), _) => Some((to, 1)),
            (None, _) => None,
        };
        if let Some((to, n)) = self.move_to
            && n >= FOLLOW_POLLS
        {
            self.move_to = None;
            tracing::info!(
                from = current,
                to,
                "Siege is on another monitor; recording there"
            );
            self.stop_ffmpeg();
            let cmd = self.spawn(now);
            self.apply(cmd, now);
        }
    }

    fn check_disk(&mut self) {
        let dir = &self.s.capture.buffer_dir;
        // RETAKE_TEST_FREE_MB_FILE names a file holding a number of MB: the drive then
        // pretends to have that much free space, to test the low-disk path without filling a
        // disk. Read on every poll, so a test can change it while Retake runs.
        let fake = std::env::var_os("RETAKE_TEST_FREE_MB_FILE")
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|mb| mb * 1024 * 1024);
        let Some(free) = fake.or_else(|| free_bytes(dir).ok()) else {
            return;
        };
        match self.disk.on_free(free) {
            Some(DiskEvent::Low) => {
                tracing::error!(
                    free_mb = free / (1024 * 1024),
                    drive = %dir.display(),
                    "disk almost full (under 2 GB free): recording stopped until there is 3 GB free"
                );
                let now = self.now();
                let cmd = self.watcher.stop_now(now);
                self.apply(cmd, now);
            }
            Some(DiskEvent::Ok) => tracing::info!("disk has space again; recording may resume"),
            None => {}
        }
    }

    /// Stops ffmpeg gracefully, then the audio pump. Returns whether ffmpeg was running and
    /// stopped cleanly.
    fn stop_ffmpeg(&mut self) -> bool {
        let clean = self.ffmpeg.take().is_some_and(|f| f.stop(GRACE));
        if let Some(a) = self.audio_ffmpeg.take() {
            a.stop(GRACE);
        }
        self.pump = None;
        clean
    }

    /// A game restarted mid-session has a new process id; process loopback on the old one
    /// would record silence, so the run restarts on the new one.
    fn follow_game_pid(&mut self, now: Ms) {
        let Some(pid) = self.audio_pid else { return };
        if self.ffmpeg.is_none() || self.game_pids.is_empty() || self.game_pids.contains(&pid) {
            return;
        }
        tracing::info!(
            old = pid,
            new = self.game_pids[0],
            "game restarted; restarting the recording for its audio"
        );
        self.stop_ffmpeg();
        let cmd = self.spawn(now);
        self.apply(cmd, now);
    }

    /// The audio tracks for a run: their pipes (created here, before ffmpeg opens them) and
    /// sources. A source that can't be resolved leaves its track silent or out, never the
    /// recording stopped.
    fn audio_tracks(&mut self, run: u64) -> Vec<Track> {
        let a = &self.s.audio;
        let mut want: Vec<(&'static str, Option<retake_audio::Source>, f32, u32)> = Vec::new();
        self.audio_pid = None;
        if a.game {
            let source = match (a.game_source, self.game_pids.first()) {
                (GameSource::Process, Some(&pid)) if process_loopback_supported() => {
                    self.audio_pid = Some(pid);
                    retake_audio::Source::ProcessLoopback { pid }
                }
                (GameSource::Process, Some(_)) => {
                    tracing::warn!(
                        "Windows is older than 10 2004: game audio is the whole output device"
                    );
                    retake_audio::Source::DeviceLoopback
                }
                _ => retake_audio::Source::DeviceLoopback,
            };
            want.push(("game", Some(source), 0.0, a.game_delay_ms));
        }
        if a.mic {
            want.push((
                "mic",
                Some(retake_audio::Source::Mic {
                    name: a.mic_device.clone(),
                }),
                a.mic_gate_db,
                0,
            ));
        }
        if a.comms {
            let discord = processes().ok().and_then(|p| {
                p.into_iter()
                    .find(|(_, n)| n.eq_ignore_ascii_case("Discord.exe"))
                    .map(|(pid, _)| pid)
            });
            match discord {
                Some(pid) => want.push((
                    "comms",
                    Some(retake_audio::Source::ProcessLoopback { pid }),
                    0.0,
                    a.game_delay_ms,
                )),
                None => tracing::info!(
                    "audio.comms is on but Discord isn't running; no comms track this run"
                ),
            }
        }
        let mut tracks = Vec::new();
        for (name, source, gate_db, delay_ms) in want {
            match OutPipe::create(&pipe_name(name, run)) {
                Ok(pipe) => tracks.push(Track {
                    name,
                    pipe,
                    source,
                    gate_db,
                    delay_ms,
                }),
                Err(e) => tracing::error!("{e:#}; recording without the {name} track"),
            }
        }
        tracks
    }

    /// Decides this session's monitor and frame rate (following `"auto"`), logs the monitor's
    /// warnings and returns the setup to record with.
    fn session_setup(&mut self, audio: Vec<AudioInput>, origin_us: i64) -> CaptureSetup {
        match crate::display::adapter() {
            Ok((_, outputs)) => self.outputs = outputs,
            Err(e) => tracing::warn!("listing monitors: {e:#}"),
        }
        let cap = &self.s.capture;
        // Only "auto" follows the game, so only then are its windows looked up.
        let gm = (cap.output == Auto::Auto)
            .then(|| game_monitor(&self.game_pids))
            .flatten();
        let MonitorChoice { output, fps, check } =
            pick_monitor(cap.output, cap.fps, &self.outputs, gm).unwrap_or(MonitorChoice {
                output: 0,
                fps: pick_fps(cap.fps, 0),
                check: Err("no monitors found on the primary GPU".into()),
            });
        self.session_warning = match check {
            Ok(warnings) => {
                for w in &warnings {
                    tracing::warn!("{w}");
                }
                !warnings.is_empty()
            }
            Err(e) => {
                tracing::warn!("{e}");
                true
            }
        };
        tracing::info!(
            output,
            fps,
            encoder = %self.s.encoder,
            game_window = gm.is_some(),
            "capture setup"
        );
        CaptureSetup {
            output,
            fps,
            encoder: self.s.encoder,
            codec: cap.codec,
            preset: cap.preset,
            qp_i: cap.qp_i,
            qp_p: cap.qp_p,
            buffer_dir: cap.buffer_dir.clone(),
            buffer_secs: cap.buffer_secs,
            audio,
            origin_us,
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
        let tracks = self.audio_tracks(self.run);
        let inputs = tracks
            .iter()
            .map(|t| AudioInput {
                pipe: t.pipe.name().to_owned(),
                title: match t.name {
                    "game" => "Game",
                    "mic" => "Mic",
                    _ => "Comms",
                },
            })
            .collect();
        // The run's origin: video timestamps and the audio pump both count from here.
        let origin_us = retake_audio::unix_us();
        let setup = self.session_setup(inputs, origin_us);
        let args = capture_args(&setup);
        tracing::info!(args = %args.join(" "), "ffmpeg command");
        self.current = Some(setup);
        let start = now_ms();
        let priority = self.s.capture.priority;
        match Ffmpeg::spawn(&self.s.job, &self.s.ffmpeg, &args, priority, on_stats) {
            Ok(f) => {
                tracing::info!(pid = f.pid(), ?priority, "ffmpeg recording");
                let audio_dir = (!tracks.is_empty())
                    .then(|| self.start_audio(tracks, origin_us))
                    .flatten();
                *self.s.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(Session {
                    run: self.run,
                    start,
                    buffer_dir: self.s.capture.buffer_dir.clone(),
                    wrap: segment_count(self.s.capture.buffer_secs),
                    live: true,
                    torn: false,
                    audio_dir,
                    game_session: self.game_session,
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

    /// Starts the audio recorder and its pump for this run. Returns the audio ring folder, or
    /// `None` (recording goes on without sound) if it couldn't start.
    fn start_audio(&mut self, tracks: Vec<Track>, origin_us: i64) -> Option<std::path::PathBuf> {
        let setup = self.current.clone()?;
        let dir = setup.buffer_dir.join(AUDIO_DIR);
        if let Err(e) = std::fs::create_dir_all(&dir).and_then(|()| clear_dir(&dir).map(|_| ())) {
            tracing::error!("preparing {}: {e}; recording without sound", dir.display());
            return None;
        }
        let args = audio_capture_args(&setup);
        tracing::info!(args = %args.join(" "), "audio ffmpeg command");
        match Ffmpeg::spawn(
            &self.s.job,
            &self.s.ffmpeg,
            &args,
            self.s.capture.priority,
            |_| {},
        ) {
            Ok(a) => {
                tracing::info!(pid = a.pid(), "audio ffmpeg recording");
                self.audio_ffmpeg = Some(a);
            }
            Err(e) => {
                tracing::error!("starting the audio ffmpeg: {e:#}; recording without sound");
                return None;
            }
        }
        match Pump::start(tracks, origin_us) {
            Ok(p) => self.pump = Some(p),
            Err(e) => tracing::error!("starting the audio pump: {e:#}"),
        }
        Some(dir)
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

    fn log_diag(&self) {
        let mb = |b: Option<u64>| b.map(|b| b / (1024 * 1024));
        let (retake_mem, retake_handles) = crate::diag::own();
        let f = self.ffmpeg.as_ref();
        tracing::info!(
            retake_mb = mb(retake_mem),
            retake_handles,
            ffmpeg_mb = mb(f.and_then(Ffmpeg::working_set)),
            ffmpeg_handles = f.and_then(Ffmpeg::handle_count),
            "diag"
        );
    }

    fn publish(&mut self) {
        let audio_lost = self.pump.as_ref().is_some_and(Pump::warning);
        let warning = self.s.warning || self.session_warning || self.speed.behind() || audio_lost;
        let mut s = tray_status(self.watcher.phase(), warning);
        if self.disk.low() {
            s = TrayStatus::Failed;
        }
        if self.shown != Some(s) {
            tracing::info!(phase = ?self.watcher.phase(), status = ?s, "status");
            (self.s.notify)(s);
            self.shown = Some(s);
        }
    }
}

/// Process loopback needs Windows 10 2004 (build 19041) or later.
fn process_loopback_supported() -> bool {
    crate::sysinfo::windows_build()
        .split('.')
        .next()
        .and_then(|b| b.parse::<u32>().ok())
        .is_some_and(|b| b >= 19_041)
}
