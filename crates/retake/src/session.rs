//! The session recording: while a game session runs, every finished segment of the ring
//! buffers (video and audio) is copied into `clips/session/<id>/`, so after the match the
//! auto-clipper can cut clips from any moment, not just the last 60 s. When the session ends,
//! `on_done` gets the folder; the auto-clipper deletes it once it's finished.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::{Context, Result};
use retake_core::Ms;
use retake_core::segments::writing_file;

use crate::clock::now_ms;
use crate::drive::free_bytes;
use crate::saver::{SessionSlot, current, read_index};

/// Below this much free space the oldest session segments are dropped.
const MIN_FREE: u64 = 20 * 1024 * 1024 * 1024;

/// One kept segment: its kind ("v" or "a"), clock-ms range and file name in the folder.
#[derive(Debug, Clone, PartialEq)]
pub struct Kept {
    pub kind: String,
    pub start: Ms,
    pub end: Ms,
    pub file: String,
}

/// Reads `session.csv` (`kind,start_ms,end_ms,file` per line).
pub fn read_kept(dir: &Path) -> Vec<Kept> {
    let Ok(text) = fs::read_to_string(dir.join("session.csv")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let mut p = l.split(',');
            Some(Kept {
                kind: p.next()?.to_owned(),
                start: p.next()?.parse().ok()?,
                end: p.next()?.parse().ok()?,
                file: p.next()?.to_owned(),
            })
        })
        .collect()
}

/// Facts about a finished session, written as `session.json`.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub dir: PathBuf,
    /// Unix ms minus clock ms: adds to a clock-ms time to get wall-clock time.
    pub unix_offset_ms: i64,
    pub start_ms: Ms,
    pub end_ms: Ms,
    /// The game's profile id (sessions from before v1.5 were Siege).
    pub game: String,
}

pub struct Keeper {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Keeper {
    /// Watches `slot` and keeps the segments of every game session in `root/<id>/`.
    pub fn start(
        slot: SessionSlot,
        root: PathBuf,
        on_done: impl Fn(SessionInfo) + Send + 'static,
    ) -> Result<Keeper> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("session-keeper".into())
            .spawn(move || run(&slot, &root, &on_done, &flag))
            .context("spawning the session keeper")?;
        Ok(Keeper {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Open {
    game_session: u64,
    dir: PathBuf,
    index: fs::File,
    start_ms: Ms,
    /// Per ring ("v", "a"): the end (clock ms) of the last segment kept.
    last_end: [Ms; 2],
    seq: u64,
    warned_space: bool,
    /// The game's profile id.
    game: String,
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn run(slot: &SessionSlot, root: &Path, on_done: &dyn Fn(SessionInfo), stop: &AtomicBool) {
    let mut open: Option<Open> = None;
    // A game session whose game has no automatic clips: nothing is kept for it.
    let mut skipped: Option<u64> = None;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_secs(1));
        let s = current(slot);
        // A new game session (not a respawn within one) starts a new folder.
        let wanted = s.as_ref().map(|s| s.game_session);
        if let Some(o) = &open
            && Some(o.game_session) != wanted
        {
            let o = open.take().expect("checked");
            finish(o, on_done);
        }
        let Some(s) = s else { continue };
        if open.is_none() {
            if !s.live || skipped == Some(s.game_session) {
                continue;
            }
            let game = crate::current_game::get();
            if !game.as_ref().is_some_and(|g| g.autoclip) {
                tracing::info!(
                    game = game.as_ref().map_or("none", |g| g.name.as_str()),
                    "no automatic clips for this game; the session isn't kept"
                );
                skipped = Some(s.game_session);
                continue;
            }
            let game = game.map(|g| g.id).unwrap_or_default();
            match start_session(root, s.game_session, game) {
                Ok(o) => open = Some(o),
                Err(e) => {
                    tracing::warn!("session recording off for this session: {e:#}");
                    continue;
                }
            }
        }
        let o = open.as_mut().expect("just opened");
        for (i, (kind, dir)) in [
            ("v", Some(s.buffer_dir.clone())),
            ("a", s.audio_dir.clone()),
        ]
        .into_iter()
        .enumerate()
        {
            let Some(dir) = dir else { continue };
            let listed = read_index(&dir, s.start);
            let writing = if s.live || s.torn {
                writing_file(&listed, s.wrap)
            } else {
                None
            };
            let since = o.last_end[i];
            for seg in listed.iter().filter(|g| g.end > since) {
                if writing.as_deref() == Some(seg.file.as_str()) {
                    break;
                }
                let name = format!("{kind}{:06}.ts", o.seq);
                o.seq += 1;
                if let Err(e) = crate::saver::copy_shared(&dir.join(&seg.file), &o.dir.join(&name))
                {
                    tracing::warn!("keeping {}: {e}", seg.file);
                    continue;
                }
                let _ = writeln!(o.index, "{kind},{},{},{name}", seg.start, seg.end);
                o.last_end[i] = seg.end;
            }
        }
        keep_space(o);
        if !s.live {
            let o = open.take().expect("open");
            finish(o, on_done);
        }
    }
    if let Some(o) = open {
        finish(o, on_done);
    }
}

fn start_session(root: &Path, game_session: u64, game: String) -> Result<Open> {
    let id = format!("{}-{game_session}", unix_ms());
    let dir = root.join(id);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let index = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("session.csv"))?;
    tracing::info!(dir = %dir.display(), "session recording started");
    Ok(Open {
        game_session,
        dir,
        index,
        start_ms: now_ms(),
        last_end: [0, 0],
        seq: 0,
        warned_space: false,
        game,
    })
}

/// Drops the oldest kept segments while the drive is short of space.
fn keep_space(o: &mut Open) {
    let Ok(free) = free_bytes(&o.dir) else { return };
    if free >= MIN_FREE {
        return;
    }
    if !o.warned_space {
        tracing::warn!("under 20 GB free: the session recording keeps only its newest part");
        o.warned_space = true;
    }
    let kept = read_kept(&o.dir);
    for k in kept.iter().take(10) {
        let _ = fs::remove_file(o.dir.join(&k.file));
    }
}

fn finish(o: Open, on_done: &dyn Fn(SessionInfo)) {
    let info = SessionInfo {
        dir: o.dir.clone(),
        unix_offset_ms: unix_ms() - now_ms() as i64,
        start_ms: o.start_ms,
        end_ms: now_ms(),
        game: o.game.clone(),
    };
    let json = serde_json::json!({
        "unix_offset_ms": info.unix_offset_ms,
        "start_ms": info.start_ms,
        "end_ms": info.end_ms,
        "game": info.game,
    });
    let _ = fs::write(o.dir.join("session.json"), json.to_string());
    tracing::info!(dir = %o.dir.display(), "session recording finished");
    on_done(info);
}

/// Reads a finished session's facts back (after a restart).
pub fn read_info(dir: &Path) -> Option<SessionInfo> {
    let v: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("session.json")).ok()?).ok()?;
    Some(SessionInfo {
        dir: dir.to_path_buf(),
        unix_offset_ms: v.get("unix_offset_ms")?.as_i64()?,
        start_ms: v.get("start_ms")?.as_u64()?,
        end_ms: v.get("end_ms")?.as_u64()?,
        game: v
            .get("game")
            .and_then(|g| g.as_str())
            .unwrap_or("siege")
            .to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_index_round_trips() {
        let d = tempfile::tempdir().unwrap();
        fs::write(
            d.path().join("session.csv"),
            "v,1000,3000,v000000.ts\na,900,3010,a000001.ts\nbad\n",
        )
        .unwrap();
        let k = read_kept(d.path());
        assert_eq!(k.len(), 2);
        assert_eq!(
            k[1],
            Kept {
                kind: "a".into(),
                start: 900,
                end: 3010,
                file: "a000001.ts".into()
            }
        );
    }
}
