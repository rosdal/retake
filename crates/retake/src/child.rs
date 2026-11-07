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
use windows::Win32::System::Threading::{
    BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW, PROCESS_CREATION_FLAGS,
};
use windows::core::PCWSTR;

/// Receives a child's output, one line at a time.
type LineSink = Box<dyn FnMut(&str) + Send>;

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
        self.spawn_inner(exe, args, Stdio::null(), None, PROCESS_CREATION_FLAGS(0))
    }

    /// Like [`Self::spawn_logged`], at below-normal priority so it yields to the game.
    pub fn spawn_logged_low<S: AsRef<OsStr>>(&self, exe: &Path, args: &[S]) -> Result<Child> {
        self.spawn_inner(exe, args, Stdio::null(), None, BELOW_NORMAL_PRIORITY_CLASS)
    }

    /// Like [`Self::spawn_logged`], but stdin is piped (ffmpeg stops on `q`) and every stderr
    /// line goes to `on_stderr` instead of the log. `priority` is a priority-class creation flag
    /// (`NORMAL_PRIORITY_CLASS` for normal: with no class flag the child inherits a below-normal
    /// or idle parent's class).
    pub fn spawn_supervised<S: AsRef<OsStr>>(
        &self,
        exe: &Path,
        args: &[S],
        priority: PROCESS_CREATION_FLAGS,
        on_stderr: impl FnMut(&str) + Send + 'static,
    ) -> Result<Child> {
        self.spawn_inner(
            exe,
            args,
            Stdio::piped(),
            Some(Box::new(on_stderr)),
            priority,
        )
    }

    fn spawn_inner<S: AsRef<OsStr>>(
        &self,
        exe: &Path,
        args: &[S],
        stdin: Stdio,
        on_stderr: Option<LineSink>,
        priority: PROCESS_CREATION_FLAGS,
    ) -> Result<Child> {
        let mut child = Command::new(exe)
            .args(args)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags((CREATE_NO_WINDOW | priority).0)
            .spawn()
            .with_context(|| format!("spawning {}", exe.display()))?;

        let process = HANDLE(child.as_raw_handle());
        // SAFETY: both handles are valid for the duration of the call.
        if let Err(e) = unsafe { AssignProcessToJobObject(self.handle, process) } {
            // A child that already finished can't be assigned; that's not a failure.
            if !matches!(child.try_wait(), Ok(Some(_))) {
                let _ = child.kill();
                bail!("AssignProcessToJobObject: {e}");
            }
        }

        let name = exe
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let pid = child.id();
        if let Some(out) = child.stdout.take() {
            forward_lines(out, format!("{name}-stdout"), log_line(pid, "stdout"));
        }
        if let Some(err) = child.stderr.take() {
            let f = on_stderr.unwrap_or_else(|| Box::new(log_line(pid, "stderr")));
            forward_lines(err, format!("{name}-stderr"), f);
        }
        tracing::info!(pid, exe = %exe.display(), "spawned child");
        Ok(child)
    }
}

impl ChildJob {
    /// Puts an already spawned child in this job, so it dies with Retake.
    pub fn adopt(&self, child: &std::process::Child) {
        // SAFETY: both handles are valid for the duration of the call.
        if let Err(e) =
            unsafe { AssignProcessToJobObject(self.handle, HANDLE(child.as_raw_handle())) }
        {
            tracing::warn!("a child runs outside Retake's job: {e}");
        }
    }

    /// Runs `exe args…` to completion in this job (so it dies with Retake), with no window, at
    /// below-normal priority, and returns its output.
    pub fn run_output<S: AsRef<OsStr>>(
        &self,
        exe: &Path,
        args: &[S],
    ) -> Result<std::process::Output> {
        let child = Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags((CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS).0)
            .spawn()
            .with_context(|| format!("spawning {}", exe.display()))?;
        // SAFETY: both handles are valid for the duration of the call.
        if let Err(e) =
            unsafe { AssignProcessToJobObject(self.handle, HANDLE(child.as_raw_handle())) }
        {
            tracing::warn!("{} runs outside Retake's job: {e}", exe.display());
        }
        child.wait_with_output().context("waiting for the child")
    }
}

impl Drop for ChildJob {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it exactly once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// Longest line forwarded in one piece; longer runs are split so a stream that never ends a
/// line can't grow the buffer without bound.
const MAX_LINE: usize = 4096;

/// Calls `f` for every non-empty line in `reader`, treating `\r`, `\n` and `\r\n` as line ends
/// (ffmpeg's progress output ends its lines with a bare `\r`).
fn for_each_line(reader: impl Read, max: usize, mut f: impl FnMut(&str)) {
    let mut reader = BufReader::new(reader);
    let mut line: Vec<u8> = Vec::with_capacity(256);
    let mut emit = |line: &mut Vec<u8>| {
        if !line.is_empty() {
            f(&String::from_utf8_lossy(line));
            line.clear();
        }
    };
    loop {
        let buf = match reader.fill_buf() {
            Ok([]) | Err(_) => break,
            Ok(buf) => buf,
        };
        for &b in buf {
            match b {
                b'\r' | b'\n' => emit(&mut line),
                _ => {
                    line.push(b);
                    if line.len() >= max {
                        // Cut before an unfinished UTF-8 sequence so no character is split.
                        let cut = match std::str::from_utf8(&line) {
                            Err(e) if e.error_len().is_none() && e.valid_up_to() > 0 => {
                                e.valid_up_to()
                            }
                            _ => line.len(),
                        };
                        let rest = line.split_off(cut);
                        emit(&mut line);
                        line = rest;
                    }
                }
            }
        }
        let n = buf.len();
        reader.consume(n);
    }
    emit(&mut line);
}

fn log_line(pid: u32, stream: &'static str) -> impl FnMut(&str) + Send + 'static {
    move |line| tracing::info!(target: "ffmpeg", pid, stream, "{line}")
}

fn forward_lines(
    pipe: impl Read + Send + 'static,
    thread: String,
    f: impl FnMut(&str) + Send + 'static,
) {
    std::thread::Builder::new()
        .name(thread)
        .spawn(move || for_each_line(pipe, MAX_LINE, f))
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

    #[test]
    fn splits_on_cr_and_lf_and_caps_length() {
        let input = b"a\rb\r\nc\n\nd\rlong-line-here".to_vec();
        let mut got = Vec::new();
        for_each_line(&input[..], 8, |l| got.push(l.to_owned()));
        assert_eq!(got, ["a", "b", "c", "d", "long-lin", "e-here"]);
    }

    #[test]
    fn cap_does_not_split_a_utf8_character() {
        let input = "abé-x\n".as_bytes(); // 'é' is 2 bytes, straddling the 3-byte cap
        let mut got = Vec::new();
        for_each_line(input, 3, |l| got.push(l.to_owned()));
        assert_eq!(got, ["ab", "é-", "x"]);
    }

    #[test]
    fn instantly_exiting_child_is_not_an_error() {
        let job = ChildJob::new().unwrap();
        for _ in 0..20 {
            let mut c = job.spawn_logged(&cmd(), &["/C", "exit 0"]).unwrap();
            c.wait().unwrap();
        }
    }

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
    fn low_priority_child_runs_below_normal() {
        use windows::Win32::System::Threading::{BELOW_NORMAL_PRIORITY_CLASS, GetPriorityClass};
        let job = ChildJob::new().unwrap();
        let mut child = job
            .spawn_logged_low(Path::new("ping.exe"), &["-n", "3", "127.0.0.1"])
            .unwrap();
        // SAFETY: our own child's handle, valid until `child` is dropped.
        let class = unsafe { GetPriorityClass(HANDLE(child.as_raw_handle())) };
        assert_eq!(class, BELOW_NORMAL_PRIORITY_CLASS.0);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn supervised_child_takes_the_given_priority() {
        use windows::Win32::System::Threading::{
            BELOW_NORMAL_PRIORITY_CLASS, GetPriorityClass, NORMAL_PRIORITY_CLASS,
        };
        let job = ChildJob::new().unwrap();
        for (flag, want) in [
            (BELOW_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS.0),
            (NORMAL_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS.0),
        ] {
            let mut child = job
                .spawn_supervised(
                    Path::new("ping.exe"),
                    &["-n", "3", "127.0.0.1"],
                    flag,
                    |_| {},
                )
                .unwrap();
            // SAFETY: our own child's handle, valid until `child` is dropped.
            let class = unsafe { GetPriorityClass(HANDLE(child.as_raw_handle())) };
            assert_eq!(class, want);
            let _ = child.kill();
            let _ = child.wait();
        }
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
