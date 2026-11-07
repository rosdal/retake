//! Montages, as pure logic: the clip library's ranking, montage projects (kept in
//! `montage.toml`, with manual changes surviving a re-arrange), and the arranger that places
//! clips on a song so every kept kill lands on a beat. Montages are only made when the user
//! starts one; clips are read, never changed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::song::Analysis;

/// A clip in the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LibClip {
    pub path: PathBuf,
    /// Unix seconds when it was saved.
    pub saved: i64,
    pub duration: f64,
    /// Kill times, seconds from the clip's start.
    pub kills: Vec<f64>,
    pub starred: bool,
    /// Montages it was used in.
    pub used: u32,
    /// The game's profile id, or empty if unknown.
    pub game: String,
}

/// How good a clip is for a montage: kills (multi-kills extra), a star, kill density, minus
/// a penalty for clips already used so new montages don't repeat old highlights.
pub fn score(c: &LibClip) -> f64 {
    let k = c.kills.len() as f64;
    let multi = if k >= 2.0 { (k - 1.0) * 1.5 } else { 0.0 };
    let density = match (c.kills.first(), c.kills.last()) {
        (Some(a), Some(b)) if c.kills.len() >= 2 => k / (b - a).max(1.0),
        _ => 0.0,
    };
    k + multi + if c.starred { 3.0 } else { 0.0 } + 2.0 * density - 2.0 * f64::from(c.used)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// 9:16, 15–35 s.
    Tiktok,
    /// 16:9, 1–4 min.
    Youtube,
}

impl Format {
    pub fn default_length(self) -> f64 {
        match self {
            Format::Tiktok => 25.0,
            Format::Youtube => 120.0,
        }
    }
    pub fn size(self) -> (u32, u32) {
        match self {
            Format::Tiktok => (1080, 1920),
            Format::Youtube => (1920, 1080),
        }
    }
    fn clip_count(self) -> usize {
        match self {
            Format::Tiktok => 8,
            Format::Youtube => 24,
        }
    }
}

/// Which clips a new montage starts from.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    Starred,
    /// The best-ranked clips.
    Auto,
    /// Every clip under this folder.
    Folder(PathBuf),
}

/// The candidate clips for a new montage, best first. Only clips with kills are useful.
pub fn candidates(lib: &[LibClip], pick: &Pick, format: Format) -> Vec<LibClip> {
    let mut c: Vec<LibClip> = lib
        .iter()
        .filter(|c| !c.kills.is_empty())
        .filter(|c| match pick {
            Pick::Starred => c.starred,
            Pick::Auto => true,
            Pick::Folder(f) => c.path.starts_with(f),
        })
        .cloned()
        .collect();
    c.sort_by(|a, b| score(b).total_cmp(&score(a)));
    if *pick == Pick::Auto {
        c.truncate(format.clip_count());
    }
    c
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Hook,
    Build,
    Drop,
    Outro,
}

/// One clip in a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjClip {
    pub file: PathBuf,
    pub include: bool,
    pub kills: Vec<f64>,
    pub duration: f64,
    pub score: f64,
    /// Set by the arranger, or by hand.
    #[serde(default)]
    pub role: Option<Role>,
    /// Source in/out points; `None` = the arranger decides.
    #[serde(default)]
    pub r#in: Option<f64>,
    #[serde(default)]
    pub out: Option<f64>,
    /// Changed by hand: re-arranging keeps its role, in/out and position.
    #[serde(default)]
    pub manual: bool,
    /// The clip's game (its look is applied before the montage's own).
    #[serde(default)]
    pub game: String,
}

/// A montage project (`montages/<date>_<name>/montage.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub format: Format,
    pub song: PathBuf,
    /// Where in the song the montage starts; `None` = put the drop at ~35%.
    #[serde(default)]
    pub song_start: Option<f64>,
    pub length: f64,
    /// Overrides for a wrong beat detection.
    #[serde(default)]
    pub bpm: Option<f64>,
    pub clips: Vec<ProjClip>,
}

impl Project {
    /// Moves clip `i` one place up (`up`) or down, by hand: from then on the build follows
    /// the project's order. Returns the clip's new index.
    pub fn move_clip(&mut self, i: usize, up: bool) -> usize {
        let j = if up {
            i.checked_sub(1)
        } else {
            Some(i + 1).filter(|&j| j < self.clips.len())
        };
        let Some(j) = j.filter(|_| i < self.clips.len()) else {
            return i;
        };
        self.clips.swap(i, j);
        for k in [i, j] {
            let c = &mut self.clips[k];
            c.manual = true;
            // A by-hand position frees the arranger's in/out for the new slot.
            c.r#in = None;
            c.out = None;
        }
        j
    }

    pub fn new(format: Format, song: &Path, clips: &[LibClip]) -> Project {
        Project {
            format,
            song: song.to_path_buf(),
            song_start: None,
            length: format.default_length(),
            bpm: None,
            clips: clips.iter().map(proj_clip).collect(),
        }
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_toml(text: &str) -> Result<Project, String> {
        toml::from_str(text).map_err(|e| e.to_string())
    }

    /// Takes a fresh candidate list but keeps everything set by hand: manual clips keep their
    /// place, role and in/out; clips the user excluded stay excluded; new candidates are added
    /// at the end.
    pub fn merge_candidates(&mut self, fresh: &[LibClip]) {
        for c in fresh {
            if !self.clips.iter().any(|p| p.file == c.path) {
                self.clips.push(proj_clip(c));
            }
        }
        for p in &mut self.clips {
            if !p.manual {
                p.role = None;
                p.r#in = None;
                p.out = None;
            }
        }
    }
}

fn proj_clip(c: &LibClip) -> ProjClip {
    ProjClip {
        file: c.path.clone(),
        include: true,
        kills: c.kills.clone(),
        duration: c.duration,
        score: score(c),
        role: None,
        r#in: None,
        out: None,
        manual: false,
        game: c.game.clone(),
    }
}

/// A straight piece of a clip's time map: output `out0..out1` plays source `src0..src1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seg {
    pub out0: f64,
    pub out1: f64,
    pub src0: f64,
    pub src1: f64,
}

impl Seg {
    pub fn speed(&self) -> f64 {
        (self.src1 - self.src0) / (self.out1 - self.out0).max(1e-9)
    }
}

/// A clip placed on the montage timeline.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub file: PathBuf,
    pub role: Role,
    pub segs: Vec<Seg>,
    /// Kept kills: (source time, output time).
    pub kills: Vec<(f64, f64)>,
}

impl Placed {
    pub fn start(&self) -> f64 {
        self.segs.first().map_or(0.0, |s| s.out0)
    }
    pub fn end(&self) -> f64 {
        self.segs.last().map_or(0.0, |s| s.out1)
    }
}

pub const MIN_SPEED: f64 = 0.25;
pub const MAX_SPEED: f64 = 2.0;
/// Speed of the deep slow-mo right after the drop's first kill.
pub const DROP_SLOWMO: f64 = 0.35;

/// The first beat (output time) at or after `t` for which getting from `src0` to `src1`
/// takes a speed within limits, preferring the one nearest real time.
fn beat_for(beats: &[f64], from_out: f64, src0: f64, src1: f64, after: f64) -> Option<f64> {
    let src = src1 - src0;
    beats
        .iter()
        .copied()
        .filter(|&b| b > after + 1e-9 && b > from_out + 1e-9)
        .filter(|&b| {
            let v = src / (b - from_out);
            (MIN_SPEED..=MAX_SPEED).contains(&v)
        })
        .min_by(|a, b| {
            let d = |x: f64| ((src / (x - from_out)).ln()).abs();
            d(*a).total_cmp(&d(*b))
        })
}

/// Places one clip from output time `t0` (a beat): each kill on its own beat, speeds within
/// limits; a kill that can't fit is left out (the clip stays). Ends on the beat after its
/// last kill. `first_on` forces the first kill onto that exact beat (the drop).
fn place(
    c: &ProjClip,
    role: Role,
    beats: &[f64],
    t0: f64,
    lead: f64,
    first_on: Option<f64>,
    limit: f64,
) -> Option<Placed> {
    let mut kills: Vec<f64> = c
        .kills
        .iter()
        .copied()
        .filter(|&k| k < c.duration)
        .collect();
    kills.sort_by(f64::total_cmp);
    // A hook is one quick kill; build clips stay short (at most two kills, close together);
    // the drop and outro keep them all.
    match role {
        Role::Hook => kills.truncate(1),
        Role::Build => {
            if let Some(&k0) = kills.first() {
                kills.retain(|&k| k - k0 <= 2.5);
                kills.truncate(2);
            }
        }
        Role::Drop | Role::Outro => {}
    }
    let first = *kills.first()?;
    // In/out points count only when set by hand; otherwise they are the last arrangement's.
    let manual_in = c.r#in.filter(|_| c.manual);
    let manual_out = c.out.filter(|_| c.manual);
    let mut src = manual_in.unwrap_or((first - lead).max(0.0));
    let mut out = t0;
    let mut segs = Vec::new();
    let mut kept = Vec::new();
    for (i, &k) in kills.iter().enumerate() {
        if k <= src {
            continue;
        }
        let target = match (i, first_on) {
            (0, Some(b)) => {
                // The drop: move the in-point so the kill lands exactly on the beat at a
                // speed within limits.
                let v = ((k - src) / (b - out).max(1e-6)).clamp(MIN_SPEED, MAX_SPEED);
                src = (k - v * (b - out)).max(0.0);
                Some(b)
            }
            _ => beat_for(beats, out, src, k, out),
        };
        let Some(b) = target.filter(|&b| b <= limit) else {
            continue;
        };
        segs.push(Seg {
            out0: out,
            out1: b,
            src0: src,
            src1: k,
        });
        kept.push((k, b));
        out = b;
        src = k;
        // Deep slow-mo for one beat after the drop's first kill.
        if role == Role::Drop && i == 0 {
            let next = beats.iter().copied().find(|&x| x > out + 1e-9);
            if let Some(nb) = next.filter(|&nb| nb <= limit) {
                let s1 = (src + DROP_SLOWMO * (nb - out)).min(c.duration);
                segs.push(Seg {
                    out0: out,
                    out1: nb,
                    src0: src,
                    src1: s1,
                });
                out = nb;
                src = s1;
            }
        }
    }
    if kept.is_empty() {
        return None;
    }
    // Tail: about half a second of source, ending on the next beat that allows it.
    let tail_end = manual_out.unwrap_or((src + 0.6).min(c.duration));
    if tail_end > src
        && let Some(b) = beat_for(beats, out, src, tail_end, out).filter(|&b| b <= limit)
    {
        segs.push(Seg {
            out0: out,
            out1: b,
            src0: src,
            src1: tail_end,
        });
    }
    Some(Placed {
        file: c.file.clone(),
        role,
        segs,
        kills: kept,
    })
}

/// Arranges the project's included clips on the song window `[start, start + length]`:
/// a quick hook, a build in rising score order, the best multi-kill with its first kill on
/// the drop, and an outro if time remains. Output times are seconds from the montage start.
pub fn arrange(p: &Project, song: &Analysis) -> Vec<Placed> {
    let start = p.song_start.unwrap_or_else(|| song.window_start(p.length));
    let beats: Vec<f64> = song
        .beats
        .iter()
        .map(|b| b - start)
        .filter(|&b| b >= -1e-9 && b <= p.length + 1e-9)
        .collect();
    let drop_out = song.drop_time() - start;
    let mut pool: Vec<&ProjClip> = p
        .clips
        .iter()
        .filter(|c| c.include && !c.kills.is_empty())
        .collect();
    if pool.is_empty() || beats.len() < 4 {
        return Vec::new();
    }
    // Manual roles first, then the arranger's choice.
    let take = |pool: &mut Vec<&ProjClip>,
                role: Role,
                pick: &dyn Fn(&&ProjClip, &&ProjClip) -> std::cmp::Ordering|
     -> Option<ProjClip> {
        let i = pool
            .iter()
            .position(|c| c.manual && c.role == Some(role))
            .or_else(|| {
                pool.iter()
                    .enumerate()
                    .filter(|(_, c)| !c.manual || c.role.is_none())
                    .max_by(|a, b| pick(a.1, b.1))
                    .map(|(i, _)| i)
            })?;
        Some(pool.remove(i).clone())
    };
    let drop = take(&mut pool, Role::Drop, &|a, b| {
        a.kills
            .len()
            .cmp(&b.kills.len())
            .then(a.score.total_cmp(&b.score))
    });
    // Hook: the single kill that happens soonest (stops the scroll fast).
    let hook = take(&mut pool, Role::Hook, &|a, b| {
        b.kills
            .len()
            .cmp(&a.kills.len())
            .then(b.kills[0].total_cmp(&a.kills[0]))
    });
    let mut build: Vec<ProjClip> = pool.iter().map(|c| (*c).clone()).collect();
    // Rising score, unless clips were reordered by hand: then the project's order.
    if !build.iter().any(|c| c.manual) {
        build.sort_by(|a, b| a.score.total_cmp(&b.score));
    }

    let mut placed = Vec::new();
    let mut t = 0.0;
    let drop_lead_room = 2.0; // seconds of output kept free before the drop for its lead-in
    if let Some(h) = &hook
        && let Some(x) = place(
            h,
            Role::Hook,
            &beats,
            t,
            0.6,
            None,
            (drop_out - drop_lead_room).max(1.0),
        )
    {
        t = x.end();
        placed.push(x);
    }
    for b in &build {
        if t >= drop_out - drop_lead_room {
            break;
        }
        if let Some(x) = place(
            b,
            Role::Build,
            &beats,
            t,
            1.0,
            None,
            drop_out - drop_lead_room,
        ) {
            t = x.end();
            placed.push(x);
        }
    }
    if let Some(d) = &drop {
        let on = beats
            .iter()
            .copied()
            .min_by(|a, b| (a - drop_out).abs().total_cmp(&(b - drop_out).abs()));
        if let Some(on) = on.filter(|&on| on > t)
            && let Some(x) = place(d, Role::Drop, &beats, t, 1.5, Some(on), p.length)
        {
            t = x.end();
            placed.push(x);
        }
    }
    // Outro: anything left that still fits.
    let rest: Vec<&ProjClip> = build
        .iter()
        .filter(|b| !placed.iter().any(|x| x.file == b.file))
        .collect();
    for b in rest {
        if p.length - t < 1.0 {
            break;
        }
        if let Some(x) = place(b, Role::Outro, &beats, t, 0.8, None, p.length) {
            t = x.end();
            placed.push(x);
        }
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lib(name: &str, kills: &[f64], starred: bool, used: u32) -> LibClip {
        LibClip {
            path: PathBuf::from(format!(r"C:\clips\raw\{name}.mkv")),
            saved: 0,
            duration: 18.0,
            kills: kills.to_vec(),
            starred,
            used,
            game: String::new(),
        }
    }

    #[test]
    fn score_prefers_multikills_stars_and_fresh_clips() {
        let one = lib("a", &[15.0], false, 0);
        let three = lib("b", &[15.0, 16.0, 17.5], false, 0);
        let starred = lib("c", &[15.0], true, 0);
        let used = lib("d", &[15.0, 16.0, 17.5], false, 3);
        assert!(score(&three) > score(&one));
        assert!(score(&starred) > score(&one));
        assert!(score(&used) < score(&three));
    }

    #[test]
    fn candidates_by_pick() {
        let l = vec![
            lib("a", &[15.0], true, 0),
            lib("b", &[15.0, 16.0], false, 0),
            lib("c", &[], true, 0),
        ];
        let s = candidates(&l, &Pick::Starred, Format::Tiktok);
        assert_eq!(s.len(), 1);
        let a = candidates(&l, &Pick::Auto, Format::Tiktok);
        assert_eq!(
            a[0].path, l[1].path,
            "best first; clips without kills left out"
        );
    }

    #[test]
    fn project_round_trips_and_keeps_manual_changes() {
        let l = vec![
            lib("a", &[15.0], false, 0),
            lib("b", &[15.0, 16.0], false, 0),
        ];
        let mut p = Project::new(Format::Tiktok, Path::new("song.mp3"), &l);
        p.clips[0].manual = true;
        p.clips[0].role = Some(Role::Hook);
        p.clips[0].r#in = Some(13.0);
        p.clips[1].include = false;
        p.clips[1].r#in = Some(2.0);
        let back = Project::from_toml(&p.to_toml()).unwrap();
        assert_eq!(back, p);
        let mut q = back;
        q.merge_candidates(&[lib("a", &[15.0], false, 0), lib("z", &[3.0], false, 0)]);
        assert_eq!(q.clips[0].r#in, Some(13.0), "manual in-point kept");
        assert_eq!(q.clips[0].role, Some(Role::Hook));
        assert!(!q.clips[1].include, "exclusion kept");
        assert_eq!(q.clips[1].r#in, None, "arranger's values cleared");
        assert_eq!(q.clips.len(), 3);
    }

    /// A 128 BPM song with the drop at bar 12.
    fn analysis() -> Analysis {
        let beat = 60.0 / 128.0;
        let beats: Vec<f64> = (0..160).map(|i| f64::from(i) * beat).collect();
        Analysis {
            bpm: 128.0,
            bars: (0..40).map(|b| b * 4).collect(),
            drop_bar: 12,
            beats,
            duration: 75.0,
        }
    }

    fn project() -> Project {
        let l = vec![
            lib("hook", &[15.2], false, 0),
            lib("b1", &[14.0], false, 0),
            lib("b2", &[12.0, 15.0], false, 0),
            lib("b3", &[15.5], false, 0),
            lib("ace", &[10.0, 11.3, 12.1, 14.0, 15.0], true, 0),
            lib("b4", &[13.0], false, 0),
        ];
        Project::new(Format::Tiktok, Path::new("song.mp3"), &l)
    }

    #[test]
    fn every_kept_kill_is_on_a_beat_and_speeds_stay_in_range() {
        let a = analysis();
        let p = project();
        let placed = arrange(&p, &a);
        assert!(!placed.is_empty());
        let start = a.window_start(p.length);
        let frame = 1.0 / 60.0;
        for x in &placed {
            for &(_, out) in &x.kills {
                let on_beat = a.beats.iter().any(|b| (b - start - out).abs() < frame);
                assert!(on_beat, "{x:?}: kill at {out:.3} is off the beat");
            }
            for s in &x.segs {
                assert!(
                    s.speed() >= MIN_SPEED - 1e-9 && s.speed() <= MAX_SPEED + 1e-9,
                    "{s:?}"
                );
            }
            assert!(x.end() <= p.length + 1e-9);
        }
        // Segments follow one another without gaps or overlaps.
        let all: Vec<Seg> = placed.iter().flat_map(|x| x.segs.clone()).collect();
        for w in all.windows(2) {
            assert!((w[1].out0 - w[0].out1).abs() < 1e-9, "{w:?}");
        }
    }

    #[test]
    fn the_best_clip_hits_the_drop_and_nothing_repeats() {
        let a = analysis();
        let p = project();
        let placed = arrange(&p, &a);
        let start = a.window_start(p.length);
        let drop = placed
            .iter()
            .find(|x| x.role == Role::Drop)
            .expect("a drop clip");
        assert!(drop.file.to_string_lossy().contains("ace"));
        assert!((drop.kills[0].1 - (a.drop_time() - start)).abs() < 1e-6);
        // Deep slow-mo right after it.
        assert!(
            drop.segs
                .iter()
                .any(|s| (s.speed() - DROP_SLOWMO).abs() < 1e-6)
        );
        let mut files: Vec<_> = placed.iter().map(|x| &x.file).collect();
        files.sort();
        files.dedup();
        assert_eq!(files.len(), placed.len());
        assert_eq!(placed[0].role, Role::Hook);
    }

    #[test]
    fn hooks_and_builds_stay_short() {
        let a = analysis();
        let mut p = project();
        // A hook candidate with kills far apart: only its first kill is used.
        p.clips[0].kills = vec![2.0, 4.0];
        let placed = arrange(&p, &a);
        assert_eq!(placed[0].role, Role::Hook);
        assert_eq!(placed[0].kills.len(), 1);
        assert!(placed.iter().filter(|x| x.role == Role::Build).count() >= 1);
        for x in placed.iter().filter(|x| x.role == Role::Build) {
            assert!(x.kills.len() <= 2);
        }
    }

    #[test]
    fn moved_clips_keep_their_order_in_the_build() {
        let a = analysis();
        let mut p = project();
        let builds = |p: &Project| -> Vec<String> {
            arrange(p, &a)
                .iter()
                .filter(|x| x.role == Role::Build)
                .map(|x| x.file.file_stem().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        let before = builds(&p);
        assert!(before.len() >= 2, "{before:?}");
        // Move the second build clip above the first.
        let pos = |p: &Project, n: &str| {
            p.clips
                .iter()
                .position(|c| c.file.file_stem().unwrap() == n)
                .unwrap()
        };
        let (i0, i1) = (pos(&p, &before[0]), pos(&p, &before[1]));
        let mut i = i1;
        while i > i0 {
            i = p.move_clip(i, true);
        }
        let after = builds(&p);
        assert_eq!(after[0], before[1], "{before:?} -> {after:?}");
        assert_eq!(p.move_clip(0, true), 0, "the top clip stays put");
    }

    #[test]
    fn manual_roles_win() {
        let a = analysis();
        let mut p = project();
        let i = p
            .clips
            .iter()
            .position(|c| c.file.to_string_lossy().contains("b2"))
            .unwrap();
        p.clips[i].manual = true;
        p.clips[i].role = Some(Role::Drop);
        let placed = arrange(&p, &a);
        let drop = placed.iter().find(|x| x.role == Role::Drop).unwrap();
        assert!(drop.file.to_string_lossy().contains("b2"));
    }
}
