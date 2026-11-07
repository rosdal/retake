//! Automatic kill clips for Siege, after the game has closed: find the Match Replays written
//! during the session, read them with the r6-dissect sidecar, find new kill-feed rows in the
//! session recording, line the two up per round and cut the player's kills (kill, 2K, 3K, 4K,
//! ACE) as clips, which then go through grading and exports like any other clip.
//!
//! Only files on disk are read, and only after the game has closed: the replay files and
//! Retake's own recording. Nothing touches the game while it runs.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use retake_core::Ms;
use retake_core::autoclip::{KillClip, MinPlay, align, clip_stem, round_clips};
use retake_core::clip::{concat_list, ts_mux_args};
use retake_core::replay::{self, Round};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::core::HSTRING;

use crate::child::ChildJob;
use crate::killfeed;
use crate::session::{Kept, SessionInfo, read_kept};

/// Siege's Steam app id and Ubisoft Connect install id.
const STEAM_APP: &str = "359550";
const UBI_INSTALL: &str = "635";
/// Kills and rows must agree within this many seconds.
const TOLERANCE: f64 = 0.5;

fn reg_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let mut buf = [0u16; 1024];
    let mut len = size_of_val(&buf) as u32;
    // SAFETY: buf and len describe a valid writable buffer.
    let rc = unsafe {
        RegGetValueW(
            root,
            &HSTRING::from(key),
            &HSTRING::from(value),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    rc.is_ok()
        .then(|| String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]))
}

/// Paths of Steam library folders from `libraryfolders.vdf`.
fn steam_libraries() -> Vec<PathBuf> {
    let Some(steam) = reg_string(HKEY_CURRENT_USER, r"Software\Valve\Steam", "SteamPath") else {
        return Vec::new();
    };
    let steam = PathBuf::from(steam.replace('/', "\\"));
    let mut libs = vec![steam.clone()];
    if let Ok(vdf) = fs::read_to_string(steam.join(r"steamapps\libraryfolders.vdf")) {
        for line in vdf.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix("\"path\"") {
                let p = rest.trim().trim_matches('"').replace("\\\\", "\\");
                libs.push(PathBuf::from(p));
            }
        }
    }
    libs
}

/// Siege's `MatchReplay` folder, from the Steam libraries or the Ubisoft Connect install entry.
pub fn replay_folder() -> Option<PathBuf> {
    for lib in steam_libraries() {
        let manifest = lib.join(format!(r"steamapps\appmanifest_{STEAM_APP}.acf"));
        if let Ok(text) = fs::read_to_string(&manifest) {
            let dir = text.lines().find_map(|l| {
                l.trim()
                    .strip_prefix("\"installdir\"")
                    .map(|r| r.trim().trim_matches('"').to_owned())
            });
            if let Some(d) = dir {
                let p = lib.join(r"steamapps\common").join(d).join("MatchReplay");
                if p.is_dir() {
                    return Some(p);
                }
            }
        }
    }
    let ubi = reg_string(
        HKEY_LOCAL_MACHINE,
        &format!(r"SOFTWARE\WOW6432Node\Ubisoft\Launcher\Installs\{UBI_INSTALL}"),
        "InstallDir",
    )?;
    let p = PathBuf::from(ubi).join("MatchReplay");
    p.is_dir().then_some(p)
}

/// Match folders (`Match-YYYY-MM-DD_HH-MM-SS-<pid>`) modified between `from` and `to`
/// (Unix ms), oldest first.
fn matches_between(root: &Path, from: i64, to: i64) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out: Vec<(i64, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("Match-") && e.path().is_dir())
        .filter_map(|e| {
            let m = e.metadata().ok()?.modified().ok()?;
            let ms = m.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64;
            (ms >= from && ms <= to).then(|| (ms, e.path()))
        })
        .collect();
    out.sort();
    out.into_iter().map(|x| x.1).collect()
}

/// Runs r6-dissect on a match folder and parses its rounds.
fn read_match(dissect: &Path, job: &ChildJob, folder: &Path, work: &Path) -> Result<Vec<Round>> {
    let out = work.join(format!(
        "{}.json",
        folder.file_name().unwrap_or_default().to_string_lossy()
    ));
    let args = [
        folder.to_string_lossy().into_owned(),
        "-o".into(),
        out.to_string_lossy().into_owned(),
    ];
    let o = job.run_output(dissect, &args)?;
    if !o.status.success() {
        bail!(
            "r6-dissect exited with {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .last()
                .unwrap_or("")
        );
    }
    let text = fs::read_to_string(&out).context("reading r6-dissect's output")?;
    replay::parse(&text).map_err(|e| anyhow::anyhow!("{e}"))
}

/// What the auto-clipper needs from the rest of Retake.
pub struct Setup {
    pub ffmpeg: PathBuf,
    /// r6-dissect.exe; `None` = not bundled (auto-clipping can't run).
    pub dissect: Option<PathBuf>,
    pub raw_dir: PathBuf,
    pub min_play: MinPlay,
    /// Called with each new clip (queues grading and exports).
    pub on_clip: Box<dyn Fn(&Path) + Send + Sync>,
    /// Called once with a message for the user (replays off, sidecar missing).
    pub on_notice: Box<dyn Fn(&str) + Send + Sync>,
}

/// Where r6-dissect is: next to retake.exe, in `r6-dissect/`, or in the dev layout.
pub fn find_dissect(exe_dir: &Path) -> Option<PathBuf> {
    [
        exe_dir.join("r6-dissect.exe"),
        exe_dir.join(r"r6-dissect\r6-dissect.exe"),
        exe_dir.join(r"..\..\third_party\r6-dissect\r6-dissect.exe"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Analyses one finished session and deletes its folder afterwards. Returns the clips made.
pub fn run(s: &Setup, info: &SessionInfo) -> Result<Vec<PathBuf>> {
    let result = analyse(s, info);
    if let Err(e) = fs::remove_dir_all(&info.dir) {
        tracing::warn!("removing {}: {e}", info.dir.display());
    }
    result
}

fn analyse(s: &Setup, info: &SessionInfo) -> Result<Vec<PathBuf>> {
    if s.min_play == MinPlay::Off {
        return Ok(Vec::new());
    }
    let minutes = (info.end_ms.saturating_sub(info.start_ms)) / 60_000;
    let Some(dissect) = &s.dissect else {
        bail!("r6-dissect isn't next to retake.exe; auto-clipping is off");
    };
    let Some(root) = replay_folder() else {
        if minutes >= 5 {
            (s.on_notice)(
                "Auto-clipping needs Siege's Match Replays: in Siege, turn on Options → General → Match Replay. Manual F9 clips still work.",
            );
        }
        bail!("no Siege MatchReplay folder found");
    };
    let from = info.start_ms as i64 + info.unix_offset_ms;
    let to = info.end_ms as i64 + info.unix_offset_ms + 60_000;
    let matches = matches_between(&root, from, to);
    if matches.is_empty() {
        if minutes >= 10 {
            (s.on_notice)(
                "No Siege Match Replay was saved during this session, so no kills were clipped automatically. Turn on Options → General → Match Replay in Siege.",
            );
        }
        return Ok(Vec::new());
    }
    let job = ChildJob::new()?;
    let kept = read_kept(&info.dir);
    let video: Vec<&Kept> = kept.iter().filter(|k| k.kind == "v").collect();
    let audio: Vec<&Kept> = kept.iter().filter(|k| k.kind == "a").collect();
    let (Some(first), Some(_)) = (video.first(), video.last()) else {
        bail!("the session recording is empty");
    };
    // Scan the whole session's kill feed in one pass over a concat list of its segments.
    let list = info.dir.join("video.txt");
    fs::write(
        &list,
        concat_list(&video.iter().map(|k| k.file.clone()).collect::<Vec<_>>()),
    )?;
    let rows: Vec<f64> = killfeed::scan_list(&s.ffmpeg, &job, &list)?
        .into_iter()
        .map(|t| first.start as f64 / 1000.0 + t)
        .collect();
    tracing::info!(
        rows = rows.len(),
        matches = matches.len(),
        "kill feed scanned"
    );

    let mut made = Vec::new();
    let mut after = 0usize; // rows already used by earlier rounds
    for m in &matches {
        let rounds = match read_match(dissect, &job, m, &info.dir) {
            Ok(r) => r,
            Err(e) => {
                (s.on_notice)(
                    "Siege update: Retake can't read this season's replays yet, so auto-clipping is paused until Retake is updated. Manual F9 clips still work.",
                );
                tracing::warn!("{}: {e:#}", m.display());
                continue;
            }
        };
        for round in &rounds {
            let kills = round.replay_kills();
            let candidates = &rows[after.min(rows.len())..];
            let at = align(&kills, candidates, TOLERANCE);
            if let Some(last) = at
                .iter()
                .flatten()
                .fold(None, |m: Option<f64>, &t| Some(m.map_or(t, |x| x.max(t))))
            {
                after += candidates
                    .iter()
                    .position(|&t| t > last)
                    .unwrap_or(candidates.len());
            }
            let details: Vec<(f64, &replay::Kill)> = kills
                .iter()
                .zip(&at)
                .zip(&round.kills)
                .filter(|((k, _), _)| k.mine)
                .filter_map(|((_, t), kill)| t.map(|t| (t, kill)))
                .collect();
            let mine: Vec<f64> = details.iter().map(|d| d.0).collect();
            let missed = kills
                .iter()
                .zip(&at)
                .filter(|(k, t)| k.mine && t.is_none())
                .count();
            if missed > 0 {
                tracing::warn!(
                    round = round.number,
                    missed,
                    "some of your kills couldn't be found in the recording"
                );
            }
            for clip in round_clips(&mine, round.enemies, s.min_play) {
                let rec = Recording {
                    info,
                    video: &video,
                    audio: &audio,
                };
                match cut(s, &job, &rec, &clip, round, &details) {
                    Ok(p) => {
                        (s.on_clip)(&p);
                        made.push(p);
                    }
                    Err(e) => tracing::warn!(round = round.number, "cutting a kill clip: {e:#}"),
                }
            }
        }
    }
    Ok(made)
}

/// Local `YYYY-MM-DD_HHMMSS` of a Unix ms time.
fn stamp(unix_ms: i64) -> String {
    use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let ft_val = (unix_ms * 10_000) + 116_444_736_000_000_000;
    let ft = FILETIME {
        dwLowDateTime: ft_val as u32,
        dwHighDateTime: (ft_val >> 32) as u32,
    };
    let (mut utc, mut local) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    // SAFETY: valid in/out structs.
    unsafe {
        let _ = FileTimeToSystemTime(&ft, &mut utc);
        let _ = SystemTimeToTzSpecificLocalTime(None, &utc, &mut local);
    }
    format!(
        "{:04}-{:02}-{:02}_{:02}{:02}{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond
    )
}

/// A finished session's recording: its facts and kept segments.
struct Recording<'a> {
    info: &'a SessionInfo,
    video: &'a [&'a Kept],
    audio: &'a [&'a Kept],
}

/// Cuts one kill clip from the session recording into `raw/` with its sidecar.
fn cut(
    s: &Setup,
    job: &ChildJob,
    rec: &Recording<'_>,
    c: &KillClip,
    round: &Round,
    details: &[(f64, &replay::Kill)],
) -> Result<PathBuf> {
    let (info, video, audio) = (rec.info, rec.video, rec.audio);
    let (from, to): (Ms, Ms) = ((c.start * 1000.0) as Ms, (c.end * 1000.0) as Ms);
    let pick = |segs: &[&Kept]| -> Vec<PathBuf> {
        segs.iter()
            .filter(|k| k.end > from && k.start < to)
            .map(|k| info.dir.join(&k.file))
            .collect()
    };
    let v = pick(video);
    if v.is_empty() {
        bail!("no recording covers {:.0}–{:.0} s", c.start, c.end);
    }
    let a = pick(audio);
    let name = clip_stem(&stamp(c.kills[0] as i64 * 1000 + info.unix_offset_ms), c);
    fs::create_dir_all(&s.raw_dir)?;
    let out = s.raw_dir.join(format!("{name}.mkv"));
    let part = s.raw_dir.join(format!("{name}.mkv.part"));
    let args = if a.is_empty() {
        retake_core::clip::ts_join_args(&v, &part)
    } else {
        ts_mux_args(&v, &a, &part)
    };
    let o = job.run_output(&s.ffmpeg, &args)?;
    if !o.status.success() {
        let _ = fs::remove_file(&part);
        bail!("ffmpeg exited with {}", o.status);
    }
    fs::rename(&part, &out)?;
    // Kill times relative to the clip's first segment.
    let base = video
        .iter()
        .find(|k| k.end > from)
        .map_or(from, |k| k.start) as f64
        / 1000.0;
    let side = serde_json::json!({
        "autoclip": {
            "play": c.play.tag(),
            "part": [c.part.0, c.part.1],
            "round": round.number,
            "map": round.map,
            "kills": details
                .iter()
                .filter(|(t, _)| c.kills.contains(t))
                .map(|(t, k)| serde_json::json!({
                    "at": t - base,
                    "victim": k.victim,
                    "headshot": k.headshot,
                }))
                .collect::<Vec<_>>(),
        }
    });
    fs::write(
        out.with_extension("json"),
        serde_json::to_string_pretty(&side)?,
    )?;
    tracing::info!(clip = %out.display(), play = %c.play.tag(), round = round.number, "kill clip saved");
    Ok(out)
}
