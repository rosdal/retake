#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio_pump;
mod background;
mod child;
mod clock;
mod diag;
mod display;
mod drive;
mod ffmpeg;
mod ffmpeg_proc;
mod instance;
mod logging;
mod pipe;
mod procs;
mod recorder;
mod saver;
mod setup_check;
mod sound;
mod sync_check;
mod sysinfo;
mod tray;
mod window;

use std::cell::RefCell;
use std::path::Path;
use std::process::{Child, ExitCode};
use std::rc::Rc;
use std::sync::{Arc, mpsc};

use anyhow::{Context, Result};
use retake_core::autoconf::{Auto, pick_drive, pick_encoder, pick_monitor};
use retake_core::buffer::{drive_problem, write_readme};
use retake_core::clip::{Kind, Press};
use retake_core::config::{self, Config, DEFAULT_CONFIG_TOML};
use retake_core::watcher::TrayStatus;
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{HSTRING, w};

use crate::background::{Background, SharedGate};
use crate::child::ChildJob;
use crate::instance::SingleInstance;
use crate::recorder::Recorder;
use crate::saver::{ClipDirs, Saver, SaverMsg, SaverSetup, SessionSlot};

struct Args {
    console: bool,
    /// `--diag`: log memory and handle counts every 5 minutes.
    diag: bool,
    /// `--save` / `--save-full`: ask the running Retake for a clip, then exit.
    save: Option<bool>,
    dummy_child: bool,
    /// Logged once logging is up; a GUI-subsystem exe has nowhere to print them earlier.
    unknown: Vec<String>,
}

fn parse_args() -> Args {
    let mut args = Args {
        console: false,
        diag: false,
        save: None,
        dummy_child: false,
        unknown: Vec::new(),
    };
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--console" => args.console = true,
            "--diag" => args.diag = true,
            "--save" => args.save = Some(false),
            "--save-full" => args.save = Some(true),
            "--dummy-child" => args.dummy_child = true,
            _ => args.unknown.push(a),
        }
    }
    args
}

/// A fatal startup error: logged, then shown in a message box because there's no console.
fn fatal(msg: &str) -> ExitCode {
    tracing::error!("{msg}");
    error_box(msg)
}

/// Shows `msg` in an error box and returns exit code 1.
fn error_box(msg: &str) -> ExitCode {
    let text = HSTRING::from(msg);
    // SAFETY: strings outlive the call.
    let rc = unsafe { MessageBoxW(None, &text, w!("Retake"), MB_OK | MB_ICONERROR) };
    if rc.0 == 0 {
        tracing::warn!(
            "MessageBoxW failed: {}",
            windows::core::Error::from_thread()
        );
    }
    ExitCode::from(1)
}

/// Writes the commented defaults if `path` doesn't exist. Returns whether it wrote them.
fn ensure_config(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(path, DEFAULT_CONFIG_TOML)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

/// `retake.exe sync-check …`: a command-line tool, not the recorder. Prints to the console it
/// was started from.
fn sync_check_main(words: &[String]) -> ExitCode {
    // SAFETY: plain Win32 calls; failure just means no console output.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            let _ = AllocConsole();
        }
    }
    let run = || -> Result<bool> {
        let args = sync_check::parse(words)?;
        let exe = std::env::current_exe().context("finding Retake's own folder")?;
        let ffmpeg = ffmpeg::locate(exe.parent().unwrap_or(Path::new(".")))?;
        let report = sync_check::run(&ffmpeg, &args)?;
        Ok(retake_core::sync::passes(&report))
    };
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("sync-check: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn main() -> ExitCode {
    let words: Vec<String> = std::env::args().skip(1).collect();
    if words.first().is_some_and(|w| w == "sync-check") {
        return sync_check_main(&words[1..]);
    }
    let args = parse_args();
    if args.console {
        // SAFETY: plain Win32 calls; failure just means no console output.
        unsafe {
            if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
                let _ = AllocConsole();
            }
        }
    }

    // Before logging, so a second launch doesn't write into the running instance's log.
    let Some(instance) = SingleInstance::acquire() else {
        if let Some(full) = args.save {
            return if instance::signal_save(full) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            };
        }
        instance::toast("Retake is already running.");
        return ExitCode::SUCCESS;
    };
    if args.save.is_some() {
        // Nothing to save from: this is the only Retake, and it has only just started.
        drop(instance);
        return ExitCode::from(1);
    }

    let guard = match logging::init(args.console) {
        Ok(g) => g,
        Err(e) => return error_box(&format!("Could not start logging: {e:#}")),
    };
    logging::log_startup_info();
    for a in &args.unknown {
        tracing::warn!("ignoring unknown argument {a}");
    }

    let exe_dir = match std::env::current_exe() {
        Ok(p) => p.parent().map(Path::to_path_buf).unwrap_or_default(),
        Err(e) => return fatal(&format!("Cannot find Retake's own folder: {e}")),
    };

    let config_path = exe_dir.join("config.toml");
    match ensure_config(&config_path) {
        Ok(true) => tracing::info!(path = %config_path.display(), "wrote default config"),
        Ok(false) => {}
        Err(e) => return fatal(&format!("{e:#}")),
    }
    let pick_root = || {
        let drives = drive::fixed_drives();
        let d = pick_drive(&drives)?;
        tracing::info!(
            drive = %d.root.display(),
            speed = %d.speed,
            free_gb = d.free / (1024 * 1024 * 1024),
            already_used = d.used,
            "auto: buffer and clips drive"
        );
        Some(d.root.clone())
    };
    let config: Config = match config::load_file(&config_path, &pick_root) {
        Ok(c) => c,
        Err(e) => return fatal(&e.to_string()),
    };
    tracing::info!(path = %config_path.display(), "config loaded");

    let (vendor, outputs) = match display::adapter() {
        Ok(a) => a,
        Err(e) => return fatal(&format!("Listing monitors: {e:#}")),
    };
    let encoder = match pick_encoder(config.capture.encoder, vendor) {
        Ok(e) => e,
        Err(e) => return fatal(&e),
    };
    tracing::info!(%encoder, %vendor, setting = ?config.capture.encoder, "encoder");

    let ffmpeg =
        match ffmpeg::locate(&exe_dir).and_then(|p| ffmpeg::self_check(&p, encoder).map(|()| p)) {
            Ok(p) => p,
            Err(e) => return fatal(&format!("{e:#}")),
        };

    let Some(choice) = pick_monitor(config.capture.output, config.capture.fps, &outputs, None)
    else {
        return fatal("No monitors found on the primary GPU");
    };
    // An explicit monitor must exist, and its warnings hold for every session. With "auto"
    // the recorder picks and checks the monitor per session: the primary's warnings here
    // could be about a monitor it never records.
    let warnings = if config.capture.output == Auto::Auto {
        Vec::new()
    } else {
        match choice.check {
            Ok(w) => w,
            Err(e) => return fatal(&e),
        }
    };
    for w in &warnings {
        tracing::warn!("{w}");
    }

    let buffer_dir = &config.capture.buffer_dir;
    if let Some(e) = drive_problem(drive::drive_kind(buffer_dir), buffer_dir) {
        return fatal(&e);
    }
    drive::warn_if_low_space(buffer_dir);
    if let Err(e) = write_readme(buffer_dir) {
        tracing::warn!("writing buffer README: {e}");
    }

    let job = match ChildJob::new() {
        Ok(j) => Arc::new(j),
        Err(e) => return fatal(&format!("{e:#}")),
    };
    let mut children: Vec<Child> = Vec::new();
    if args.dummy_child {
        match job.spawn_logged(
            &ffmpeg,
            &[
                "-hide_banner",
                "-f",
                "lavfi",
                "-i",
                "nullsrc",
                "-f",
                "null",
                "-",
            ],
        ) {
            Ok(c) => children.push(c),
            Err(e) => tracing::error!("dummy child: {e:#}"),
        }
    }

    let dirs = ClipDirs::new(&config.clips.out_dir);
    if let Err(e) = dirs.prepare() {
        return fatal(&format!(
            "Preparing {}: {e}",
            config.clips.out_dir.display()
        ));
    }
    let session: SessionSlot = Arc::default();

    // Both strings were validated with the config, so a failure here is a bug.
    let mut specs = Vec::new();
    let mut kinds = Vec::new();
    for (label, kind) in [
        (&config.clips.hotkey, Kind::Normal),
        (&config.clips.hotkey_full, Kind::Full),
    ] {
        match retake_core::hotkey::parse(label) {
            Ok(key) => {
                specs.push(tray::HotkeySpec {
                    label: label.clone(),
                    key,
                });
                kinds.push(kind);
            }
            Err(e) => return fatal(&format!("clips hotkey {label}: {e}")),
        }
    }
    let (press_tx, press_rx) = mpsc::channel();
    let cue = config.clips.sound_cue;
    let hotkey_tx = press_tx.clone();
    let hotkey_session = session.clone();
    let on_hotkey = move |i: usize| {
        let Some(&kind) = kinds.get(i) else { return };
        let p = Press {
            at: clock::now_ms(),
            stamp: clock::local_stamp(),
            kind,
        };
        // Still sent when not recording: a press just after the game closed can save its end.
        if hotkey_tx.send(SaverMsg::Press(p)).is_err() {
            tracing::warn!("saver is not running; press ignored");
            return;
        }
        // The cue promises a clip, so it only plays while something is being recorded.
        let live = saver::current(&hotkey_session).is_some_and(|s| s.live);
        if !live {
            tracing::info!("clip hotkey while not recording");
        } else if cue {
            sound::play_press();
        }
    };

    // Game-time quiet mode: background work (the setup check, later grading and exports)
    // only runs while Siege is closed.
    let quiet: SharedGate = Arc::default();
    let background = match Background::start(quiet.clone()) {
        Ok(b) => b,
        Err(e) => return fatal(&format!("{e:#}")),
    };
    let jobs = background.submitter();
    let check_gate = quiet.clone();
    jobs.submit("setup check", move || setup_check::run(&check_gate));

    let paths = tray::TrayPaths {
        config: config_path,
        clips: config.clips.out_dir.clone(),
        logs: logging::log_dir(),
    };
    let recorder: Rc<RefCell<Option<Recorder>>> = Rc::default();
    let slot = recorder.clone();
    let saver: Rc<RefCell<Option<Saver>>> = Rc::default();
    let saver_slot = saver.clone();
    let setup_job = job.clone();
    let capture = config.capture.clone();
    let audio = config.audio.clone();
    let diag = args.diag;
    let processes = config.game.processes.clone();
    let (pre_roll, post_roll) = (config.clips.pre_roll, config.clips.post_roll);
    let warning = !warnings.is_empty();
    let on_ready = move |notifier: tray::TrayNotifier| {
        let saver_setup = SaverSetup {
            ffmpeg: ffmpeg.clone(),
            job: setup_job.clone(),
            dirs,
            session: session.clone(),
            pre_roll,
            post_roll,
            buffer_secs: capture.buffer_secs,
            on_saved: Box::new(move |p: &Path| {
                notifier.clip_saved(p.to_path_buf());
                if cue {
                    sound::play_saved();
                }
            }),
            on_failed: Box::new(|why: &str| instance::toast(&format!("Clip not saved: {why}"))),
        };
        match Saver::start(saver_setup, press_tx, press_rx) {
            Ok(s) => *saver_slot.borrow_mut() = Some(s),
            Err(e) => tracing::error!("{e:#}"),
        }
        let setup = recorder::Setup {
            ffmpeg,
            encoder,
            capture,
            audio,
            diag,
            processes,
            warning,
            job: setup_job,
            notify: Box::new(move |s| notifier.set(s)),
            session,
            quiet: quiet.clone(),
            on_session_end: Box::new(move || {
                let gate = quiet.clone();
                jobs.submit("setup check", move || setup_check::run(&gate));
            }),
        };
        match Recorder::start(setup) {
            Ok(r) => {
                *slot.borrow_mut() = Some(r);
                let power_slot = slot.clone();
                tray::set_power_handler(move |suspending| {
                    if let Some(r) = power_slot.borrow().as_ref() {
                        r.power(suspending);
                    }
                });
            }
            Err(e) => {
                tracing::error!("{e:#}");
                notifier.set(TrayStatus::Failed);
            }
        }
    };
    let shutdown = move || {
        tracing::info!(children = children.len(), "shutting down");
        // The saver first: it reads the buffer the recorder is about to stop writing.
        drop(saver.borrow_mut().take());
        if let Some(r) = recorder.borrow_mut().take() {
            r.shutdown();
        }
        drop(background);
        for mut c in children {
            let _ = c.kill();
            let _ = c.wait();
        }
        drop(job);
        drop(instance);
        tracing::info!("bye");
        drop(guard); // flushes the log file
    };

    let on_hotkey = Arc::new(on_hotkey);
    let remote = on_hotkey.clone();
    if let Err(e) = instance::listen_for_saves(move |i| {
        tracing::info!(full = i == 1, "clip requested by retake.exe --save");
        remote(i);
    }) {
        tracing::warn!("retake.exe --save won't work: {e}");
    }
    match tray::run(paths, specs, move |i| on_hotkey(i), on_ready, shutdown) {
        Ok(()) => ExitCode::SUCCESS,
        // Already logged by tray::run, whose exit hook has closed the log.
        Err(e) => error_box(&format!("{e:#}")),
    }
}
