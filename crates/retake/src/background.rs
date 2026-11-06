//! Background jobs under game-time quiet mode: queued work runs one job at a time, and a job
//! starts only when the [`QuietGate`] allows it (Siege not running, closed for 30 s). A job
//! already running when Siege starts finishes first; nothing new starts until the gate opens.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use retake_core::quiet::QuietGate;

use crate::clock::now_ms;

/// The quiet-mode gate, shared by the recorder (which updates it) and the runner.
pub type SharedGate = Arc<Mutex<QuietGate>>;

/// Whether background work may start now.
pub fn gate_open(gate: &SharedGate) -> bool {
    gate.lock()
        .unwrap_or_else(|e| e.into_inner())
        .may_start(now_ms())
}

type Job = (String, Box<dyn FnOnce() + Send>);

/// Queues jobs from any thread.
#[derive(Clone)]
pub struct Submitter(Sender<Job>);

impl Submitter {
    pub fn submit(&self, name: &str, job: impl FnOnce() + Send + 'static) {
        if self.0.send((name.to_owned(), Box::new(job))).is_err() {
            tracing::warn!(job = name, "background runner stopped; job dropped");
        }
    }
}

/// The runner thread. Dropping it drops queued jobs and waits for a running one.
pub struct Background {
    tx: Sender<Job>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Background {
    pub fn start(gate: SharedGate) -> Result<Background> {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("background".into())
            .spawn(move || run(&gate, &rx, &flag))
            .context("spawning background thread")?;
        Ok(Background {
            tx,
            stop,
            thread: Some(thread),
        })
    }

    pub fn submitter(&self) -> Submitter {
        Submitter(self.tx.clone())
    }
}

impl Drop for Background {
    /// Submitters elsewhere keep the channel open, so the thread is stopped by a flag it
    /// checks every tick.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take()
            && t.join().is_err()
        {
            tracing::error!("background thread panicked");
        }
    }
}

fn run(gate: &SharedGate, rx: &Receiver<Job>, stop: &AtomicBool) {
    let mut queue: VecDeque<Job> = VecDeque::new();
    let mut open = None;
    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(job) => queue.push_back(job),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        queue.extend(rx.try_iter());
        let now_open = gate_open(gate);
        if open != Some(now_open) {
            open = Some(now_open);
            if !now_open {
                tracing::info!(queued = queue.len(), "quiet mode: background work paused");
            } else if !queue.is_empty() {
                tracing::info!(
                    queued = queue.len(),
                    "quiet mode over: background work resumes"
                );
            }
        }
        if now_open
            && !stop.load(Ordering::Relaxed)
            && let Some((name, job)) = queue.pop_front()
        {
            tracing::info!(job = %name, "background job");
            // One bad job must not end background work for the rest of the run.
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                tracing::error!(job = %name, "background job panicked");
            }
        }
    }
    if !queue.is_empty() {
        tracing::info!(
            dropped = queue.len(),
            "quitting; queued background jobs dropped"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use retake_core::quiet::RESUME_AFTER;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Instant;

    fn wait_for(cond: impl Fn() -> bool, max: Duration) -> bool {
        let start = Instant::now();
        while start.elapsed() < max {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        cond()
    }

    #[test]
    fn jobs_wait_for_the_gate_and_run_in_order() {
        let gate: SharedGate = Arc::default();
        gate.lock().unwrap().on_game(true, now_ms());
        let bg = Background::start(gate.clone()).unwrap();
        let order = Arc::new(Mutex::new(Vec::new()));
        let ran = Arc::new(AtomicU32::new(0));
        for i in 0..2 {
            let (order, ran) = (order.clone(), ran.clone());
            bg.submitter().submit("test", move || {
                order.lock().unwrap().push(i);
                ran.fetch_add(1, Ordering::SeqCst);
            });
        }
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(
            ran.load(Ordering::SeqCst),
            0,
            "ran while the game was running"
        );
        gate.lock().unwrap().on_game(false, now_ms() - RESUME_AFTER);
        assert!(wait_for(
            || ran.load(Ordering::SeqCst) == 2,
            Duration::from_secs(3)
        ));
        assert_eq!(*order.lock().unwrap(), [0, 1]);
        drop(bg);
    }

    #[test]
    fn a_panicking_job_does_not_stop_later_ones() {
        let gate: SharedGate = Arc::default();
        gate.lock().unwrap().on_game(false, now_ms());
        let bg = Background::start(gate).unwrap();
        let ran = Arc::new(AtomicU32::new(0));
        bg.submitter().submit("bad", || panic!("job failed"));
        let r = ran.clone();
        bg.submitter().submit("good", move || {
            r.fetch_add(1, Ordering::SeqCst);
        });
        assert!(wait_for(
            || ran.load(Ordering::SeqCst) == 1,
            Duration::from_secs(3)
        ));
    }

    #[test]
    fn dropping_the_runner_stops_it_even_while_submitters_live() {
        let bg = Background::start(Arc::default()).unwrap();
        let kept = bg.submitter();
        let start = Instant::now();
        drop(bg);
        assert!(start.elapsed() < Duration::from_secs(2));
        kept.submit("late", || {}); // only logs
    }
}
