//! What the window reads: the service's status file, the clip library (read-only: the service
//! is its one writer), montage projects, thumbnails, song waveforms and the window's own
//! preferences.

use std::path::{Path, PathBuf};

use retake_core::control::Status;
use retake_core::montage::{LibClip, Project};
use serde::{Deserialize, Serialize};

pub fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The service's status, or `None` if the file is missing or unreadable.
pub fn read_status(path: &Path) -> Option<Status> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// A clip as the library screen shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Clip {
    pub lib: LibClip,
    pub thumb: Option<PathBuf>,
    pub strip: Option<PathBuf>,
    /// "manual", "kill", "3k", "ace", "full" from the sidecar or the name.
    pub play: String,
}

impl Clip {
    pub fn name(&self) -> String {
        self.lib
            .path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

fn play_of(p: &Path) -> String {
    std::fs::read_to_string(p.with_extension("json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v.pointer("/autoclip/play")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| {
            if p.to_string_lossy().ends_with("_full.mkv") {
                "full".into()
            } else {
                "manual".into()
            }
        })
}

/// Every clip in the library, newest first, with its thumbnails if made.
pub fn read_library(clips_dir: &Path) -> Vec<Clip> {
    let db = clips_dir.join("library.db");
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return Vec::new();
    };
    let has_game = conn.prepare("SELECT game FROM clips LIMIT 0").is_ok();
    let sql = if has_game {
        "SELECT path, saved, duration, kills, starred, used, game FROM clips ORDER BY saved DESC"
    } else {
        "SELECT path, saved, duration, kills, starred, used, '' FROM clips ORDER BY saved DESC"
    };
    let Ok(mut st) = conn.prepare(sql) else {
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
    let thumbs = clips_dir.join("thumbs");
    rows.map(|r| r.flatten().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.path.is_file())
        .map(|lib| {
            let stem = lib
                .path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let t = thumbs.join(format!("{stem}.jpg"));
            let s = thumbs.join(format!("{stem}_strip.jpg"));
            Clip {
                play: play_of(&lib.path),
                thumb: t.is_file().then_some(t),
                strip: s.is_file().then_some(s),
                lib,
            }
        })
        .collect()
}

/// The 8 frames of a hover strip, as RGBA images.
pub fn split_strip(path: &Path) -> Option<Vec<iced::widget::image::Handle>> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let n = 8;
    let fw = w / n;
    if fw == 0 {
        return None;
    }
    Some(
        (0..n)
            .map(|i| {
                let f = image::imageops::crop_imm(&img, i * fw, 0, fw, h).to_image();
                iced::widget::image::Handle::from_rgba(fw, h, f.into_raw())
            })
            .collect(),
    )
}

/// A montage project in `clips/montages`.
#[derive(Debug, Clone, PartialEq)]
pub struct Montage {
    pub dir: PathBuf,
    pub project: Project,
}

impl Montage {
    pub fn name(&self) -> String {
        self.dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }

    pub fn save(&self) -> Result<(), String> {
        std::fs::write(self.dir.join("montage.toml"), self.project.to_toml())
            .map_err(|e| e.to_string())
    }

    pub fn analysis(&self) -> Option<retake_core::song::Analysis> {
        serde_json::from_str(&std::fs::read_to_string(self.dir.join("song.json")).ok()?).ok()
    }
}

pub fn read_montages(clips_dir: &Path) -> Vec<Montage> {
    let Ok(rd) = std::fs::read_dir(clips_dir.join("montages")) else {
        return Vec::new();
    };
    let mut v: Vec<Montage> = rd
        .flatten()
        .filter_map(|e| {
            let dir = e.path();
            let text = std::fs::read_to_string(dir.join("montage.toml")).ok()?;
            Some(Montage {
                project: Project::from_toml(&text).ok()?,
                dir,
            })
        })
        .collect();
    v.sort_by(|a, b| b.dir.cmp(&a.dir));
    v
}

/// Peak levels (0–1) of a song in `n` buckets, cached as `waveform.json` in the project.
pub fn waveform(ffmpeg: &Path, song: &Path, cache: &Path, n: usize) -> Vec<f32> {
    if let Ok(t) = std::fs::read_to_string(cache)
        && let Ok(v) = serde_json::from_str::<Vec<f32>>(&t)
        && v.len() == n
    {
        return v;
    }
    let out = crate::platform::quiet_command(ffmpeg)
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(song)
        .args(["-ac", "1", "-ar", "2000", "-f", "s16le", "-"])
        .output();
    let Ok(out) = out else { return Vec::new() };
    let samples: Vec<i16> = out
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();
    if samples.is_empty() {
        return Vec::new();
    }
    let per = samples.len().div_ceil(n);
    let mut peaks: Vec<f32> = samples
        .chunks(per)
        .map(|c| c.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0) as f32 / 32768.0)
        .collect();
    peaks.resize(n, 0.0);
    let _ = std::fs::write(cache, serde_json::to_string(&peaks).unwrap_or_default());
    peaks
}

/// The window's own preferences (`%LOCALAPPDATA%\Retake\ui.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub intro_seen: bool,
    pub reduce_motion: bool,
}

impl Prefs {
    fn path() -> PathBuf {
        crate::platform::app_data().join("ui.json")
    }

    pub fn load() -> Prefs {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
    }
}

/// A game id's display name.
pub fn game_name(id: &str) -> String {
    if id.is_empty() {
        return "Unknown game".into();
    }
    retake_core::games::BUILTIN
        .iter()
        .find(|p| p.id == id)
        .map_or_else(|| id.to_owned(), |p| p.name.to_owned())
}

/// "Today 21:04", "Yesterday 18:30" or "3 Nov 20:12" for a Unix time.
pub fn when(unix_s: i64, now_s: i64) -> String {
    let day = |t: i64| t.div_euclid(86_400);
    let local = |t: i64| t + local_offset_s();
    let (t, now) = (local(unix_s), local(now_s));
    let hm = format!(
        "{:02}:{:02}",
        t.rem_euclid(86_400) / 3600,
        t.rem_euclid(3600) / 60
    );
    match day(now) - day(t) {
        0 => format!("Today {hm}"),
        1 => format!("Yesterday {hm}"),
        _ => {
            let (y, m, d) = civil(day(t));
            const M: [&str; 12] = [
                "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
            ];
            let _ = y;
            format!("{d} {} {hm}", M[(m - 1) as usize])
        }
    }
}

/// Days since 1970-01-01 → (year, month, day).
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn local_offset_s() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut tz = TIME_ZONE_INFORMATION::default();
    // SAFETY: valid out-struct.
    let r = unsafe { GetTimeZoneInformation(&mut tz) };
    let bias = tz.Bias
        + if r == 2 {
            tz.DaylightBias
        } else {
            tz.StandardBias
        };
    -i64::from(bias) * 60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_399), (2025, 11, 7));
    }

    #[test]
    fn prefs_default_to_showing_the_intro() {
        let p: Prefs = serde_json::from_str("{}").unwrap();
        assert!(!p.intro_seen && !p.reduce_motion);
    }
}
