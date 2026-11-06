#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod child;
mod ffmpeg;
mod instance;
mod logging;
mod sysinfo;
mod tray;

use std::path::Path;
use std::process::{Child, ExitCode};

use anyhow::{Context, Result};
use retake_core::config::{self, Config, DEFAULT_CONFIG_TOML};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole};
use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::{HSTRING, w};

use crate::child::ChildJob;
use crate::instance::SingleInstance;

struct Args {
    console: bool,
    dummy_child: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        console: false,
        dummy_child: false,
    };
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--console" => args.console = true,
            "--dummy-child" => args.dummy_child = true,
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    args
}

/// A fatal startup error: logged, then shown in a message box because there's no console.
fn fatal(msg: &str) -> ExitCode {
    tracing::error!("{msg}");
    let text = HSTRING::from(msg);
    // SAFETY: strings outlive the call.
    unsafe { MessageBoxW(None, &text, w!("Retake"), MB_OK | MB_ICONERROR) };
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

    let guard = match logging::init(args.console) {
        Ok(g) => g,
        Err(e) => return fatal(&format!("Could not start logging: {e:#}")),
    };
    logging::log_startup_info();

    let Some(instance) = SingleInstance::acquire() else {
        tracing::info!("another instance is running; exiting");
        instance::toast("Retake is already running.");
        return ExitCode::SUCCESS;
    };

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

    let job = match ChildJob::new() {
        Ok(j) => j,
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
    let shutdown = move || {
        tracing::info!(children = children.len(), "shutting down");
        for mut c in children {
            let _ = c.kill();
            let _ = c.wait();
        }
        drop(job);
        drop(instance);
        tracing::info!("bye");
        drop(guard); // flushes the log file
    };

    match tray::run(paths, shutdown) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fatal(&format!("{e:#}")),
    }
}
