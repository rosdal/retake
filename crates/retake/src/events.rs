//! Live kill events from games with an official event interface, logged for the auto-clipper:
//! - CS2: Valve's Game State Integration posts the game state to `127.0.0.1:3417`; Retake
//!   writes the `.cfg` that asks for it into CS2's cfg folder while CS2 is closed.
//! - League of Legends: Riot's Live Client Data API on `127.0.0.1:2999`, polled while League
//!   runs.
//!
//! Neither reads anything from the game: the game itself sends or serves the data. Kills are
//! appended to `clips/session/events.csv` (`unix_ms,game,round`); the clips are still cut
//! after the match, in quiet mode. Times in the log are Unix ms (Retake's own clock restarts
//! with every launch).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use retake_core::{gsi, liveclient};

/// Milliseconds since the Unix epoch.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

const LOG: &str = "events.csv";

/// A logged kill.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Unix ms.
    pub ms: i64,
    pub game: String,
    /// The round (CS2), or 0.
    pub round: u32,
}

/// Appends kills to the log in `root` (`clips/session`).
pub fn log(root: &Path, game: &str, round: u32, times: &[i64]) {
    if times.is_empty() {
        return;
    }
    let _ = std::fs::create_dir_all(root);
    let r = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join(LOG))
        .and_then(|mut f| {
            for t in times {
                writeln!(f, "{t},{game},{round}")?;
            }
            Ok(())
        });
    match r {
        Ok(()) => tracing::info!(game, round, kills = times.len(), "kill event"),
        Err(e) => tracing::warn!("logging a kill event: {e}"),
    }
}

/// The logged kills of `game` between two Unix ms times.
pub fn read(root: &Path, game: &str, from: i64, to: i64) -> Vec<Event> {
    let Ok(text) = std::fs::read_to_string(root.join(LOG)) else {
        return Vec::new();
    };
    let mut v: Vec<Event> = text
        .lines()
        .filter_map(|l| {
            let mut p = l.split(',');
            Some(Event {
                ms: p.next()?.parse().ok()?,
                game: p.next()?.to_owned(),
                round: p.next()?.parse().ok()?,
            })
        })
        .filter(|e| e.game == game && e.ms >= from && e.ms <= to)
        .collect();
    v.sort_by_key(|e| e.ms);
    v
}

/// Drops log lines older than `keep_from` (Unix ms) once no session needs them.
pub fn trim(root: &Path, keep_from: i64) {
    let path = root.join(LOG);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return;
    };
    let kept: String = text
        .lines()
        .filter(|l| {
            l.split(',')
                .next()
                .and_then(|t| t.parse::<i64>().ok())
                .is_some_and(|t| t >= keep_from)
        })
        .map(|l| format!("{l}\n"))
        .collect();
    let _ = std::fs::write(path, kept);
}

// ---- CS2 -------------------------------------------------------------------------------

/// CS2's cfg folders in every Steam library.
fn cs2_cfg_dirs() -> Vec<PathBuf> {
    crate::autoclipper::steam_libraries()
        .into_iter()
        .map(|l| l.join(r"steamapps\common\Counter-Strike Global Offensive\game\csgo\cfg"))
        .filter(|d| d.is_dir())
        .collect()
}

/// The token CS2 sends back, kept next to the event log so it survives restarts.
fn token(root: &Path) -> String {
    let path = root.join("gsi-token");
    if let Ok(t) = std::fs::read_to_string(&path)
        && t.trim().len() >= 16
    {
        return t.trim().to_owned();
    }
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
    );
    h.write_u32(std::process::id());
    let mut h2 = std::collections::hash_map::RandomState::new().build_hasher();
    h2.write_u64(h.finish());
    let t = format!("{:016x}{:016x}", h.finish(), h2.finish());
    let _ = std::fs::create_dir_all(root);
    let _ = std::fs::write(&path, &t);
    t
}

/// Writes (or updates) Retake's GSI file in CS2's cfg folders. Only while CS2 is closed: CS2
/// reads it at start, and its files are never touched while it runs. Returns the folders
/// written.
pub fn install_cs2_cfg(root: &Path, cs2_running: bool) -> Vec<PathBuf> {
    if cs2_running {
        return Vec::new();
    }
    let text = gsi::cfg_text(&token(root));
    let mut written = Vec::new();
    for d in cs2_cfg_dirs() {
        let f = d.join(gsi::CFG_NAME);
        if std::fs::read_to_string(&f).is_ok_and(|t| t == text) {
            continue;
        }
        match std::fs::write(&f, &text) {
            Ok(()) => written.push(f),
            Err(e) => tracing::warn!("writing {}: {e}", f.display()),
        }
    }
    written
}

/// One HTTP request's body (GSI posts are small; at most 1 MB is read).
fn read_post(stream: &mut TcpStream) -> Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    let mut r = BufReader::new(stream);
    let mut len = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            bail!("connection closed in the headers");
        }
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().context("content-length")?;
        }
    }
    if len > 1 << 20 {
        bail!("post too large");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Handles one GSI post: logs the player's new kills if CS2 is the current game.
fn handle_post(root: &Path, token: &str, tracker: &mut gsi::Tracker, body: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return;
    };
    if !gsi::authorised(&v, token) {
        return;
    }
    if let Some(k) = tracker.on_post(&v) {
        log(root, "cs2", k.round, &vec![unix_now(); k.count as usize]);
    }
}

/// Listens for CS2's posts on 127.0.0.1 for the life of the program.
pub fn start_gsi(root: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("cs2-gsi".into())
        .spawn(move || {
            let listener = match TcpListener::bind(("127.0.0.1", gsi::PORT)) {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!(
                        "CS2 events: port {} is taken ({e}); CS2 auto-clips are off",
                        gsi::PORT
                    );
                    return;
                }
            };
            let token = token(&root);
            let mut tracker = gsi::Tracker::default();
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                match read_post(&mut s) {
                    Ok(body) => handle_post(&root, &token, &mut tracker, &body),
                    Err(e) => tracing::debug!("GSI request: {e:#}"),
                }
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("starting the CS2 event listener: {e}");
    }
}

// ---- League of Legends -----------------------------------------------------------------

/// GET `https://127.0.0.1:2999/liveclientdata/allgamedata`. The game serves it with Riot's
/// own certificate, which isn't in Windows' store, so certificate errors are ignored for this
/// one localhost request.
fn get_allgamedata() -> Result<String> {
    let mut body = Vec::new();
    crate::net::get(
        &crate::net::Get {
            host: liveclient::URL_HOST,
            port: liveclient::URL_PORT,
            path: liveclient::URL_PATH,
            any_certificate: true,
            max_bytes: 8 << 20,
            timeout_ms: 2_000,
        },
        &mut body,
    )?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

/// Polls League's Live Client Data API every second while League is the current game.
pub fn start_league(root: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("league-events".into())
        .spawn(move || {
            let mut tracker: Option<liveclient::Tracker> = None;
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let playing =
                    crate::current_game::get().is_some_and(|g| g.id == "lol" && g.autoclip);
                if !playing {
                    tracker = None;
                    continue;
                }
                // The API answers only during a match (not in the client or loading screen).
                let Ok(body) = get_allgamedata() else {
                    continue;
                };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) else {
                    continue;
                };
                let t = tracker.get_or_insert_with(liveclient::Tracker::default);
                let now = unix_now();
                let times: Vec<i64> = t
                    .on_poll(&v)
                    .into_iter()
                    .map(|ago| now - (ago * 1000.0) as i64)
                    .collect();
                log(&root, "lol", 0, &times);
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("starting the League event poller: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_log_round_trips_and_filters() {
        let d = tempfile::tempdir().unwrap();
        log(d.path(), "cs2", 3, &[1000, 1000]);
        log(d.path(), "lol", 0, &[1500]);
        log(d.path(), "cs2", 4, &[5000]);
        let e = read(d.path(), "cs2", 0, 4000);
        assert_eq!(e.len(), 2);
        assert_eq!(
            e[0],
            Event {
                ms: 1000,
                game: "cs2".into(),
                round: 3
            }
        );
        assert_eq!(read(d.path(), "lol", 0, i64::MAX).len(), 1);
        trim(d.path(), 2000);
        assert_eq!(read(d.path(), "cs2", 0, i64::MAX).len(), 1);
    }

    /// Recorded GSI posts replayed into a real listener socket.
    #[test]
    fn replayed_gsi_posts_become_kills() {
        let d = tempfile::tempdir().unwrap();
        let tok = token(d.path());
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let posts: Vec<String> = [(0, 0), (1, 0), (3, 2)]
            .iter()
            .map(|(k, r)| {
                serde_json::json!({
                    "provider": {"steamid": "765"},
                    "map": {"name": "de_inferno", "round": r},
                    "round": {"phase": "live"},
                    "player": {"steamid": "765", "match_stats": {"kills": k}},
                    "auth": {"token": tok}
                })
                .to_string()
            })
            .chain([serde_json::json!({"auth": {"token": "wrong"}, "provider": {"steamid": "765"}, "player": {"steamid": "765", "match_stats": {"kills": 9}}}).to_string()])
            .collect();
        let sender = std::thread::spawn(move || {
            for p in posts {
                let mut s = TcpStream::connect(addr).unwrap();
                write!(
                    s,
                    "POST / HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{p}",
                    p.len()
                )
                .unwrap();
                let mut resp = String::new();
                let _ = s.read_to_string(&mut resp);
                assert!(resp.starts_with("HTTP/1.1 200"));
            }
        });
        let mut tracker = gsi::Tracker::default();
        for _ in 0..4 {
            let (mut s, _) = listener.accept().unwrap();
            let body = read_post(&mut s).unwrap();
            handle_post(d.path(), &tok, &mut tracker, &body);
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .unwrap();
        }
        sender.join().unwrap();
        let e = read(d.path(), "cs2", 0, i64::MAX);
        assert_eq!(e.iter().map(|e| e.round).collect::<Vec<_>>(), [1, 3, 3]);
    }

    #[test]
    fn the_cs2_file_is_never_written_while_cs2_runs() {
        let d = tempfile::tempdir().unwrap();
        assert!(install_cs2_cfg(d.path(), true).is_empty());
    }
}
