//! The game being played right now, as the recorder detected it: read by the session keeper,
//! the library and the event listeners.

use std::sync::Mutex;

use retake_core::games::Game;

static CURRENT: Mutex<Option<Game>> = Mutex::new(None);

pub fn set(g: Option<Game>) {
    *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = g;
}

pub fn get() -> Option<Game> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
