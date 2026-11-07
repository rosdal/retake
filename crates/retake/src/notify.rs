//! Toasts that never appear during a match: while the game runs they are queued and shown
//! once it has closed (nothing is drawn over the game).

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static GAME_RUNNING: AtomicBool = AtomicBool::new(false);
static QUEUED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Shows `msg` now, or after the game closes if it's running.
pub fn toast(msg: &str) {
    if GAME_RUNNING.load(Ordering::Relaxed) {
        let mut q = QUEUED.lock().unwrap_or_else(|e| e.into_inner());
        if !q.iter().any(|m| m == msg) {
            q.push(msg.to_owned());
        }
        tracing::info!("toast held until the game closes: {msg}");
    } else {
        crate::instance::toast(msg);
    }
}

/// Called by the recorder on every poll; shows held toasts once the game has closed.
pub fn game_running(running: bool) {
    let was = GAME_RUNNING.swap(running, Ordering::Relaxed);
    if was && !running {
        let held: Vec<String> =
            std::mem::take(&mut *QUEUED.lock().unwrap_or_else(|e| e.into_inner()));
        for m in held {
            crate::instance::toast(&m);
        }
    }
}
