#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod child;
mod clock;
mod display;
mod drive;
mod ffmpeg;
mod ffmpeg_proc;
mod instance;
mod logging;
mod procs;
mod recorder;
mod saver;
mod sysinfo;
mod tray;

use std::cell::RefCell;
use std::path::Path;
use std::process::{Child, ExitCode};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context, Result};
use retake_core::buffer::{drive_problem, write_readme};
use retake_core::config::{self, Config, DEFAULT_CONFIG_TOML};
use retake_core::display::check_output;
use retake_core::watcher::TrayStatus;
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{HSTRING, w};

use crate::child::ChildJob;
use crate::instance::SingleInstance;
use crate::recorder::Recorder;

struct Args {
    console: bool,
    dummy_child: bool,
    /// Logged once logging is up; a GUI-subsystem exe has nowhere to print them earlier.
    unknown: Vec<String>,
}

fn parse_args() -> Args {
    let mut args = Args {
        console: false,
        dummy_child: false,
        unknown: Vec::new(),
    };
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--console" => args.console = true,
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

fn main() -> ExitCode {
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
        instance::toast("Retake is already running.");
        return ExitCode::SUCCESS;
    };

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
    let config: Config = match config::load_file(&config_path) {
        Ok(c) => c,
        Err(e) => return fatal(&e.to_string()),
    };
    tracing::info!(path = %config_path.display(), "config loaded");

    let ffmpeg = match ffmpeg::locate(&exe_dir).and_then(|p| ffmpeg::self_check(&p).map(|()| p)) {
        Ok(p) => p,
        Err(e) => return fatal(&format!("{e:#}")),
    };

    let outputs = match display::outputs() {
        Ok(o) => o,
        Err(e) => return fatal(&format!("Listing monitors: {e:#}")),
    };
    let warnings = match check_output(&outputs, config.capture.output, config.capture.fps) {
        Ok(w) => w,
        Err(e) => return fatal(&e),
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

    let paths = tray::TrayPaths {
        config: config_path,
        clips: config.clips.out_dir.clone(),
        logs: logging::log_dir(),
    };
    let recorder: Rc<RefCell<Option<Recorder>>> = Rc::default();
    let slot = recorder.clone();
    let setup_job = job.clone();
    let capture = config.capture.clone();
    let processes = config.game.processes.clone();
    let warning = !warnings.is_empty();
    let on_ready = move |notifier: tray::TrayNotifier| {
        let setup = recorder::Setup {
            ffmpeg,
            capture,
            processes,
            warning,
            job: setup_job,
            notify: Box::new(move |s| notifier.set(s)),
        };
        match Recorder::start(setup) {
            Ok(r) => *slot.borrow_mut() = Some(r),
            Err(e) => {
                tracing::error!("{e:#}");
                notifier.set(TrayStatus::Failed);
            }
        }
    };
    let shutdown = move || {
        tracing::info!(children = children.len(), "shutting down");
        if let Some(r) = recorder.borrow_mut().take() {
            r.shutdown();
        }
        for mut c in children {
            let _ = c.kill();
            let _ = c.wait();
        }
        drop(job);
        drop(instance);
        tracing::info!("bye");
        drop(guard); // flushes the log file
    };

    match tray::run(paths, on_ready, shutdown) {
        Ok(()) => ExitCode::SUCCESS,
        // Already logged by tray::run, whose exit hook has closed the log.
        Err(e) => error_box(&format!("{e:#}")),
    }
}
