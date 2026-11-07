//! The game being played right now, as the recorder detected it, and its process ids: read by
//! the session keeper, the library, the event listeners and the status file.

use std::sync::Mutex;

use retake_core::games::Game;

static CURRENT: Mutex<Option<Game>> = Mutex::new(None);
static PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

pub fn set(g: Option<Game>) {
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = g;
}

pub fn get() -> Option<Game> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn set_pids(p: Vec<u32>) {
    *PIDS.lock().unwrap_or_else(|e| e.into_inner()) = p;
}

pub fn pids() -> Vec<u32> {
    PIDS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
