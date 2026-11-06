//! Spawning child processes (ffmpeg) safely: no console window, output piped into `tracing`,
//! and every child in a Job Object that kills it when Retake's handle closes — including
//! when Retake itself is killed.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use anyhow::{Context, Result, bail};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::core::PCWSTR;

/// A Job Object with `KILL_ON_JOB_CLOSE`. Every process spawned through it dies when this
/// value is dropped, or when the OS closes the handle because Retake exited or was killed.
pub struct ChildJob {
    handle: HANDLE,
}

// SAFETY: a job handle is a kernel object handle; using it from any thread is fine.
unsafe impl Send for ChildJob {}
unsafe impl Sync for ChildJob {}

impl ChildJob {
    pub fn new() -> Result<Self> {
        // SAFETY: plain Win32 calls; `info` outlives the call that reads it.
        unsafe {
            let handle = CreateJobObjectW(None, PCWSTR::null()).context("CreateJobObjectW")?;
            let job = ChildJob { handle };
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .context("SetInformationJobObject")?;
            Ok(job)
        }
    }

    /// Spawns `exe args…` with no window, assigns it to this job and forwards each line of
    /// its stdout and stderr to `tracing` under the target `ffmpeg`.
    pub fn spawn_logged<S: AsRef<OsStr>>(&self, exe: &Path, args: &[S]) -> Result<Child> {
        let mut child = Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW.0)
            .spawn()
            .with_context(|| format!("spawning {}", exe.display()))?;

        let process = HANDLE(child.as_raw_handle());
        // SAFETY: both handles are valid for the duration of the call.
        if let Err(e) = unsafe { AssignProcessToJobObject(self.handle, process) } {
            let _ = child.kill();
            bail!("AssignProcessToJobObject: {e}");
        }

        let name = exe
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let pid = child.id();
        if let Some(out) = child.stdout.take() {
            forward_lines(out, name.clone(), pid, "stdout");
        }
        if let Some(err) = child.stderr.take() {
            forward_lines(err, name, pid, "stderr");
        }
        tracing::info!(pid, exe = %exe.display(), "spawned child");
        Ok(child)
    }
}

impl Drop for ChildJob {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it exactly once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

fn forward_lines(pipe: impl Read + Send + 'static, name: String, pid: u32, stream: &'static str) {
    std::thread::Builder::new()
        .name(format!("{name}-{stream}"))
        .spawn(move || {
            for line in BufReader::new(pipe).lines() {
                match line {
                    Ok(line) => tracing::info!(target: "ffmpeg", pid, stream, "{line}"),
                    Err(_) => break,
                }
            }
        })
        .expect("spawning log forwarder thread");
}

/// Runs `exe args…` to completion with no window and returns its stdout (lossy UTF-8).
/// Fails if it can't start or exits non-zero.
pub fn run_capture<S: AsRef<OsStr>>(exe: &Path, args: &[S]) -> Result<String> {
    let out = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()
        .with_context(|| format!("running {}", exe.display()))?;
    if !out.status.success() {
        bail!(
            "{} exited with {}: {}",
            exe.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cmd() -> PathBuf {
        PathBuf::from(std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".into()))
    }

    #[test]
    fn run_capture_returns_stdout() {
        let out = run_capture(&cmd(), &["/C", "echo hi"]).unwrap();
        assert_eq!(out.trim(), "hi");
    }

    #[test]
    fn spawn_logged_child_exit_code() {
        let job = ChildJob::new().unwrap();
        let mut child = job.spawn_logged(&cmd(), &["/C", "exit 3"]).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(3));
    }

    #[test]
    fn dropping_job_kills_children() {
        let job = ChildJob::new().unwrap();
        // ping with a long count keeps running for ~30 s unless killed.
        let mut child = job
            .spawn_logged(Path::new("ping.exe"), &["-n", "30", "127.0.0.1"])
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(
            child.try_wait().unwrap().is_none(),
            "child should still be running"
        );
        let start = std::time::Instant::now();
        drop(job);
        let status = child.wait().unwrap();
        eprintln!("killed child status: {status:?}");
        assert!(start.elapsed().as_secs() < 5, "child outlived the job");
    }
}
