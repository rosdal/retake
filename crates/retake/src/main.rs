#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio_pump;
mod autoclipper;
mod autostart;
mod background;
mod child;
mod clock;
mod config_watch;
mod diag;
mod diagnostics;
mod display;
mod drive;
mod ffmpeg;
mod ffmpeg_proc;
mod grader;
mod instance;
mod killfeed;
mod library;
mod logging;
mod media;
mod montage;
mod notify;
mod pipe;
mod procs;
mod recorder;
mod saver;
mod session;
mod setup_check;
mod sound;
mod status;
mod sync_check;
mod sysinfo;
mod tray;
mod ui;
mod window;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
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
    /// `--autostart`: started with Windows; wait before watching for the game.
    autostart: bool,
    /// `--restarted`: relaunched to apply a new config; wait for the old instance to quit.
    restarted: bool,
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
        autostart: false,
        restarted: false,
        save: None,
        dummy_child: false,
        unknown: Vec::new(),
    };
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--console" => args.console = true,
            "--diag" => args.diag = true,
            autostart::ARG => args.autostart = true,
            config_watch::RESTARTED => args.restarted = true,
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

/// For the command-line tools in this GUI-subsystem exe: print to the console they were
/// started from, unless output is already redirected (to a file or pipe).
fn attach_console() {
    use windows::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    // SAFETY: plain Win32 calls; failure just means no console output.
    unsafe {
        let redirected =
            GetStdHandle(STD_OUTPUT_HANDLE).is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
        if !redirected && AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            let _ = AllocConsole();
        }
    }
}

/// `retake.exe sync-check …`: a command-line tool, not the recorder. Prints to the console it
/// was started from.
fn sync_check_main(words: &[String]) -> ExitCode {
    attach_console();
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

/// `retake.exe grade <clip>… [--export <preset>]…`: grades clips (and makes exports) right
/// away with the settings in `config.toml`, without the recorder. Prints to the console.
fn grade_main(words: &[String]) -> ExitCode {
    attach_console();
    let run = || -> Result<()> {
        let mut clips = Vec::new();
        let mut presets = Vec::new();
        let mut it = words.iter();
        while let Some(w) = it.next() {
            if w == "--export" {
                let p = it.next().context("--export needs a preset")?;
                presets.push(
                    retake_core::export::Preset::parse(p)
                        .with_context(|| format!("unknown preset {p}"))?,
                );
            } else {
                clips.push(std::path::PathBuf::from(w));
            }
        }
        if clips.is_empty() {
            anyhow::bail!(
                "usage: retake grade <clip>... [--export edit|edit-smooth|share|vertical]..."
            );
        }
        let exe = std::env::current_exe().context("finding Retake''s own folder")?;
        let dir = exe.parent().unwrap_or(Path::new("."));
        ensure_config(&dir.join("config.toml"))?;
        let pick_root = || drive::fixed_drives().first().map(|d| d.root.clone());
        let config = config::load_file(&dir.join("config.toml"), &pick_root)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let (vendor, _) = display::adapter()?;
        let encoder =
            pick_encoder(config.capture.encoder, vendor).map_err(|e| anyhow::anyhow!(e))?;
        let g = grader::Grader {
            ffmpeg: ffmpeg::locate(dir)?,
            encoder,
            grade: config.grade.clone(),
            blur: config.motion_blur.clone(),
            out_dir: config.clips.out_dir.clone(),
        };
        for clip in &clips {
            let t = std::time::Instant::now();
            if config.grade.mode == retake_core::config::GradeMode::On {
                let out = g.grade(clip)?;
                println!(
                    "graded {} -> {} in {:.1} s",
                    clip.display(),
                    out.display(),
                    t.elapsed().as_secs_f32()
                );
            }
            for &p in &presets {
                let t = std::time::Instant::now();
                let out = g.export(clip, p)?;
                println!(
                    "exported {} -> {} in {:.1} s",
                    p.name(),
                    out.display(),
                    t.elapsed().as_secs_f32()
                );
            }
        }
        Ok(())
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("grade: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    let words: Vec<String> = std::env::args().skip(1).collect();
    if words.first().is_some_and(|w| w == "sync-check") {
        return sync_check_main(&words[1..]);
    }
    if words.first().is_some_and(|w| w == "grade") {
        return grade_main(&words[1..]);
    }
    if words.first().is_some_and(|w| w == "montage") {
        attach_console();
        let r = (|| -> Result<()> {
            let exe = std::env::current_exe()?;
            let dir = exe.parent().unwrap_or(Path::new("."));
            ensure_config(&dir.join("config.toml"))?;
            let pick_root = || drive::fixed_drives().first().map(|d| d.root.clone());
            let config = config::load_file(&dir.join("config.toml"), &pick_root)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let (vendor, _) = display::adapter()?;
            let encoder =
                pick_encoder(config.capture.encoder, vendor).map_err(|e| anyhow::anyhow!(e))?;
            montage::cli(
                &ffmpeg::locate(dir)?,
                encoder,
                &config.clips.out_dir,
                &words[1..],
            )
        })();
        return match r {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("montage: {e:#}");
                ExitCode::from(1)
            }
        };
    }
    if words.first().is_some_and(|w| w == "write-look") {
        // The built-in look as a .cube file (for the release zip, or to edit in Resolve).
        let out = words
            .get(1)
            .map_or_else(|| PathBuf::from("siege.cube"), PathBuf::from);
        let cube = retake_core::grade::builtin_look(33).write("Retake Siege look");
        if let Some(dir) = out.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        return match std::fs::write(&out, cube) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(1),
        };
    }
    if words.first().is_some_and(|w| w == "scan-feed") {
        attach_console();
        let exe = std::env::current_exe().unwrap_or_default();
        let dir = exe.parent().unwrap_or(Path::new("."));
        let r = ffmpeg::locate(dir).and_then(|f| killfeed::scan_main(&f, &words[1..]));
        return match r {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("scan-feed: {e:#}");
                ExitCode::from(1)
            }
        };
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
    // A relaunch waits (up to 15 s) for the old instance to let go of the lock.
    let mut instance = SingleInstance::acquire();
    if args.restarted {
        for _ in 0..60 {
            if instance.is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
            instance = SingleInstance::acquire();
        }
    }
    let Some(instance) = instance else {
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
    let ui_press_tx = press_tx.clone();
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
    let strict = config.safety.strict;
    if strict {
        tracing::info!(
            "strict safety: device audio loopback, the game's settings file is not read"
        );
    }
    let check_gate = quiet.clone();
    if !strict {
        jobs.submit("setup check", move || setup_check::run(&check_gate));
    }

    // Grading and exports: queued per saved clip, run only while the game is closed.
    let grader = grader::Grader {
        ffmpeg: ffmpeg.clone(),
        encoder,
        grade: config.grade.clone(),
        blur: config.motion_blur.clone(),
        out_dir: config.clips.out_dir.clone(),
    };
    if let Err(e) = grader.look() {
        return fatal(&format!("grade.look: {e:#}"));
    }
    let media = media::Media::start(
        grader,
        jobs.clone(),
        config.export.when,
        config.export.presets.clone(),
        config.clips.max_raw_gb,
        notify::toast,
    );
    let saved_media = media.clone();

    // Auto-clipping (Siege): keep the whole session's recording, then after the game closes
    // read its Match Replays and cut the player's kills. Off when not asked for or not Siege:
    // keeping the session doubles the disk writes while recording.
    let is_siege = config
        .game
        .processes
        .iter()
        .any(|p| p.to_ascii_lowercase().starts_with("rainbowsix"));
    let auto_on =
        config.autoclip.enabled && config.autoclip.clip != config::ClipPlays::Off && is_siege;
    let session_root = config.clips.out_dir.join("session");
    let _keeper = if auto_on {
        let told = std::sync::atomic::AtomicBool::new(false);
        let clip_media = media.clone();
        let setup = Arc::new(autoclipper::Setup {
            ffmpeg: ffmpeg.clone(),
            dissect: autoclipper::find_dissect(&exe_dir),
            raw_dir: config.clips.out_dir.join("raw"),
            min_play: config.autoclip.clip.min_play(),
            on_clip: Box::new(move |p: &Path| clip_media.clip_saved(p)),
            on_notice: Box::new(move |m: &str| {
                if !told.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    notify::toast(m);
                }
            }),
        });
        if setup.dissect.is_none() {
            tracing::warn!("r6-dissect isn't next to retake.exe: auto-clipping can't read replays");
        }
        let submit = {
            let jobs = jobs.clone();
            let setup = setup.clone();
            move |info: session::SessionInfo| {
                let setup = setup.clone();
                jobs.submit("auto-clip", move || match autoclipper::run(&setup, &info) {
                    Ok(clips) => tracing::info!(clips = clips.len(), "auto-clipping done"),
                    Err(e) => tracing::warn!("auto-clipping: {e:#}"),
                });
            }
        };
        // Sessions left from an earlier run (Retake quit before analysing them).
        if let Ok(entries) = std::fs::read_dir(&session_root) {
            for e in entries.flatten() {
                if let Some(info) = session::read_info(&e.path()) {
                    submit(info);
                }
            }
        }
        match session::Keeper::start(session.clone(), session_root, submit) {
            Ok(k) => Some(k),
            Err(e) => {
                tracing::warn!("{e:#}; auto-clipping is off");
                None
            }
        }
    } else {
        None
    };

    let diag_config = config_path.clone();
    let watch_config = config_path.clone();
    ui::init(ui::UiSetup {
        config: config_path.clone(),
        clips: config.clips.out_dir.clone(),
        logs: logging::log_dir(),
    });
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
    let mut audio = config.audio.clone();
    if config.safety.strict {
        audio.game_source = config::GameSource::Device;
    }
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
                saved_media.clip_saved(p);
                let name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                status::update(|b| {
                    b.clips_today += 1;
                    b.clips_session += 1;
                });
                status::event(format!("Clip saved  {name}"));
            }),
            on_failed: Box::new(|why: &str| {
                status::event(format!("Clip not saved: {why}"));
                notify::toast(&format!("Clip not saved: {why}"));
            }),
        };
        match Saver::start(saver_setup, press_tx, press_rx) {
            Ok(s) => *saver_slot.borrow_mut() = Some(s),
            Err(e) => tracing::error!("{e:#}"),
        }
        let action_recorder = slot.clone();
        tray::set_media_handler(move |a| match a {
            tray::MediaAction::ExportNow => media.export_now(),
            tray::MediaAction::Regrade(p) => media.regrade(&p),
            tray::MediaAction::Export(p, preset) => media.export(&p, &preset),
            tray::MediaAction::Pause(on) => {
                if let Some(r) = action_recorder.borrow().as_ref() {
                    r.pause(on);
                }
            }
            tray::MediaAction::SaveFull => {
                let p = Press {
                    at: clock::now_ms(),
                    stamp: clock::local_stamp(),
                    kind: Kind::Full,
                };
                if ui_press_tx.send(SaverMsg::Press(p)).is_err() {
                    tracing::warn!("saver is not running; save ignored");
                }
            }
            tray::MediaAction::Autostart(on) => match autostart::set(on) {
                Ok(()) => notify::toast(if on {
                    "Retake now starts with Windows."
                } else {
                    "Retake no longer starts with Windows."
                }),
                Err(e) => notify::toast(&format!("Couldn't change Start with Windows: {e}")),
            },
            tray::MediaAction::CopyDiagnostics => match diagnostics::copy(&diag_config) {
                Ok(()) => notify::toast("Diagnostics copied to the clipboard."),
                Err(e) => notify::toast(&format!("Couldn't copy diagnostics: {e:#}")),
            },
        });
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
                if !strict {
                    let gate = quiet.clone();
                    jobs.submit("setup check", move || setup_check::run(&gate));
                }
            }),
            start_delay_ms: if args.autostart {
                autostart::DELAY_MS
            } else {
                0
            },
        };
        let watch_notifier = notifier;
        config_watch::start(watch_config.clone(), move || watch_notifier.quit());
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
