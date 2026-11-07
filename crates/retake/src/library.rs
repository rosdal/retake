//! The clip library: `clips/library.db` (SQLite) with every saved clip, its kill times, star
//! and how often montages used it. Filled on every save; "Rescan library" rebuilds it from the
//! folder and the sidecars.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use retake_core::montage::LibClip;
use rusqlite::{Connection, params};

use crate::grader::probe;

/// Seconds from a manual clip's end to its press: where its play most likely is.
const MANUAL_KILL_FROM_END: f64 = 3.0;

pub struct Library {
    db: Mutex<Connection>,
    raw: PathBuf,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Kill times of a clip from its sidecar (auto-clips), or one estimated kill near the end of
/// a manual clip, where the hotkey was pressed.
fn kills_for(clip: &Path, duration: f64) -> Vec<f64> {
    let from_sidecar = std::fs::read_to_string(clip.with_extension("json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| {
            v.pointer("/autoclip/kills")
                .and_then(|k| k.as_array())
                .map(|k| {
                    k.iter()
                        .filter_map(|x| {
                            x.get("at")
                                .and_then(serde_json::Value::as_f64)
                                .or_else(|| x.as_f64())
                        })
                        .collect::<Vec<f64>>()
                })
        });
    match from_sidecar {
        Some(k) if !k.is_empty() => k,
        _ if duration > MANUAL_KILL_FROM_END + 1.0 => vec![duration - MANUAL_KILL_FROM_END],
        _ => Vec::new(),
    }
}

/// The game recorded in a clip's sidecar (`"game"` at the top or under `autoclip`).
pub fn game_of(clip: &Path) -> Option<String> {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(clip.with_extension("json")).ok()?).ok()?;
    v.get("game")
        .or_else(|| v.pointer("/autoclip/game"))
        .and_then(|g| g.as_str())
        .filter(|g| !g.is_empty())
        .map(str::to_owned)
}

impl Library {
    pub fn open(clips: &Path) -> Result<Library> {
        std::fs::create_dir_all(clips)?;
        let db = Connection::open(clips.join("library.db")).context("opening library.db")?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS clips (
                path TEXT PRIMARY KEY,
                saved INTEGER NOT NULL,
                duration REAL NOT NULL,
                kills TEXT NOT NULL,
                starred INTEGER NOT NULL DEFAULT 0,
                used INTEGER NOT NULL DEFAULT 0
            );",
        )?;
        // v1.5: the game. Older libraries get the column added.
        let has_game = db.prepare("SELECT game FROM clips LIMIT 0").is_ok();
        if !has_game {
            db.execute_batch("ALTER TABLE clips ADD COLUMN game TEXT NOT NULL DEFAULT ''")?;
        }
        Ok(Library {
            db: Mutex::new(db),
            raw: clips.join("raw"),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds or refreshes a clip (keeps its star and use count). Its game comes from the
    /// sidecar, or is the game running now (a clip saved just now).
    pub fn add(&self, ffmpeg: &Path, clip: &Path) -> Result<()> {
        let game = game_of(clip)
            .or_else(|| crate::current_game::get().map(|g| g.id))
            .unwrap_or_default();
        self.add_with_game(ffmpeg, clip, &game)
    }

    fn add_with_game(&self, ffmpeg: &Path, clip: &Path, game: &str) -> Result<()> {
        let duration = probe(ffmpeg, clip).map_or(0.0, |p| p.duration);
        let kills = kills_for(clip, duration);
        let saved = std::fs::metadata(clip)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or_else(now_unix, |d| d.as_secs() as i64);
        self.conn().execute(
            "INSERT INTO clips (path, saved, duration, kills, game) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET duration = excluded.duration, kills = excluded.kills,
                 game = CASE WHEN excluded.game = '' THEN clips.game ELSE excluded.game END",
            params![
                clip.to_string_lossy(),
                saved,
                duration,
                serde_json::to_string(&kills)?,
                game
            ],
        )?;
        Ok(())
    }

    /// Every clip, newest first; clips whose file is gone are dropped from the library.
    pub fn list(&self) -> Vec<LibClip> {
        let conn = self.conn();
        let Ok(mut st) = conn.prepare(
            "SELECT path, saved, duration, kills, starred, used, game FROM clips ORDER BY saved DESC",
        ) else {
            return Vec::new();
        };
        let rows = st.query_map([], |r| {
            Ok(LibClip {
                path: PathBuf::from(r.get::<_, String>(0)?),
                saved: r.get(1)?,
                duration: r.get(2)?,
                kills: serde_json::from_str(&r.get::<_, String>(3)?).unwrap_or_default(),
                starred: r.get::<_, i64>(4)? != 0,
                used: r.get::<_, i64>(5)? as u32,
                game: r.get(6)?,
            })
        });
        let all: Vec<LibClip> = rows.map(|r| r.flatten().collect()).unwrap_or_default();
        let (keep, gone): (Vec<_>, Vec<_>) = all.into_iter().partition(|c| c.path.is_file());
        for g in gone {
            let _ = conn.execute(
                "DELETE FROM clips WHERE path = ?1",
                params![g.path.to_string_lossy()],
            );
        }
        keep
    }

    /// Rebuilds the library from `clips/raw` and the sidecars. Returns the number of clips.
    pub fn rescan(&self, ffmpeg: &Path) -> Result<usize> {
        let mut n = 0;
        for e in std::fs::read_dir(&self.raw)?.flatten() {
            let p = e.path();
            let name = p
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if p.extension().is_some_and(|x| x == "mkv") && !name.ends_with("_graded") {
                // Rescans never guess from the game running now.
                self.add_with_game(ffmpeg, &p, &game_of(&p).unwrap_or_default())?;
                n += 1;
            }
        }
        let _ = self.list();
        Ok(n)
    }

    /// Stars (or unstars) a clip.
    pub fn set_star(&self, clip: &Path, on: bool) -> Result<()> {
        self.conn().execute(
            "UPDATE clips SET starred = ?2 WHERE path = ?1",
            params![clip.to_string_lossy(), i64::from(on)],
        )?;
        Ok(())
    }

    /// Stars the newest clip (the star hotkey). Returns it.
    pub fn star_last(&self) -> Result<Option<PathBuf>> {
        let newest = self.list().into_iter().next().map(|c| c.path);
        if let Some(p) = &newest {
            self.set_star(p, true)?;
        }
        Ok(newest)
    }

    /// Counts a use in a montage for each clip.
    pub fn mark_used(&self, clips: &[PathBuf]) -> Result<()> {
        let conn = self.conn();
        for c in clips {
            conn.execute(
                "UPDATE clips SET used = used + 1 WHERE path = ?1",
                params![c.to_string_lossy()],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_kills_or_a_manual_estimate() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a_3K.mkv");
        std::fs::write(
            a.with_extension("json"),
            r#"{"autoclip":{"kills":[{"at":15.0},{"at":16.2},{"at":17.0}]}}"#,
        )
        .unwrap();
        assert_eq!(kills_for(&a, 21.0), [15.0, 16.2, 17.0]);
        assert_eq!(kills_for(&d.path().join("b.mkv"), 18.0), [15.0]);
        assert!(kills_for(&d.path().join("c.mkv"), 2.0).is_empty());
    }

    #[test]
    fn stars_and_uses_survive_a_refresh() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::open(d.path()).unwrap();
        let raw = d.path().join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        let clip = raw.join("x.mkv");
        std::fs::write(&clip, b"not a video").unwrap();
        // ffmpeg isn't needed: an unreadable clip gets duration 0.
        let no_ffmpeg = d.path().join("ffmpeg.exe");
        lib.add(&no_ffmpeg, &clip).unwrap();
        assert_eq!(lib.star_last().unwrap(), Some(clip.clone()));
        lib.mark_used(std::slice::from_ref(&clip)).unwrap();
        lib.add(&no_ffmpeg, &clip).unwrap();
        let l = lib.list();
        assert_eq!(l.len(), 1);
        assert!(l[0].starred);
        assert_eq!(l[0].used, 1);
        std::fs::remove_file(&clip).unwrap();
        assert!(lib.list().is_empty(), "missing files leave the library");
    }

    #[test]
    fn the_game_comes_from_the_sidecar_and_survives_refreshes() {
        let d = tempfile::tempdir().unwrap();
        let lib = Library::open(d.path()).unwrap();
        let raw = d.path().join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        let clip = raw.join("k.mkv");
        std::fs::write(&clip, b"x").unwrap();
        std::fs::write(
            clip.with_extension("json"),
            r#"{"game":"cs2","autoclip":{"kills":[]}}"#,
        )
        .unwrap();
        let no_ffmpeg = d.path().join("ffmpeg.exe");
        lib.rescan(&no_ffmpeg).unwrap();
        assert_eq!(lib.list()[0].game, "cs2");
        // A refresh without a known game keeps the recorded one.
        std::fs::remove_file(clip.with_extension("json")).unwrap();
        lib.add_with_game(&no_ffmpeg, &clip, "").unwrap();
        assert_eq!(lib.list()[0].game, "cs2");
    }

    #[test]
    fn old_libraries_get_the_game_column() {
        let d = tempfile::tempdir().unwrap();
        let c = Connection::open(d.path().join("library.db")).unwrap();
        c.execute_batch(
            "CREATE TABLE clips (path TEXT PRIMARY KEY, saved INTEGER NOT NULL, duration REAL NOT NULL,
             kills TEXT NOT NULL, starred INTEGER NOT NULL DEFAULT 0, used INTEGER NOT NULL DEFAULT 0);",
        )
        .unwrap();
        drop(c);
        let lib = Library::open(d.path()).unwrap();
        assert!(lib.list().is_empty());
    }
}
