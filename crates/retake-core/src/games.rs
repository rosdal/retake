//! Games Retake knows: a built-in database of profiles (exe names, anti-cheat, how kills are
//! found, where the kill feed is), user overrides from `[games.<id>]`, and detection from the
//! names in a process snapshot. Nothing here opens a game's process.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The anti-cheat a game ships with; decides Retake's defaults for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AntiCheat {
    BattlEye,
    /// Riot Vanguard, kernel level: strict safety by default.
    Vanguard,
    EasyAntiCheat,
    Vac,
    Ricochet,
    /// Another or an unknown anti-cheat.
    Other,
    None,
}

impl AntiCheat {
    /// Whether strict safety (device audio only, never reading the game's files) is the
    /// default for games with this anti-cheat.
    pub fn strict_by_default(self) -> bool {
        matches!(self, AntiCheat::Vanguard)
    }

    pub fn name(self) -> &'static str {
        match self {
            AntiCheat::BattlEye => "BattlEye",
            AntiCheat::Vanguard => "Riot Vanguard",
            AntiCheat::EasyAntiCheat => "Easy Anti-Cheat",
            AntiCheat::Vac => "VAC",
            AntiCheat::Ricochet => "Ricochet",
            AntiCheat::Other => "anti-cheat",
            AntiCheat::None => "none",
        }
    }
}

/// How kills are found for automatic clips, safest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Siege: the game's own Match Replay files, read after the match (v0.8).
    SiegeReplay,
    /// CS2: Valve's Game State Integration posts the game state to Retake.
    Gsi,
    /// League of Legends: Riot's local Live Client Data API.
    LiveClient,
    /// The kill feed in Retake's own recording, scanned after the match.
    KillFeed,
    /// `F9` and stars only.
    Manual,
}

impl Method {
    /// Kills arrive while the game runs (and are logged), rather than being found afterwards.
    pub fn live_events(self) -> bool {
        matches!(self, Method::Gsi | Method::LiveClient)
    }
}

/// A rectangle as fractions of the frame: `(x, y, w, h)`, so it holds on any resolution.
pub type Rect = (f64, f64, f64, f64);

/// A game's built-in profile.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    pub id: &'static str,
    pub name: &'static str,
    /// Exe file names (matched case-insensitively).
    pub exes: &'static [&'static str],
    pub anticheat: AntiCheat,
    pub method: Method,
    /// Where the kill feed is, for kill-feed scanning and the vertical montage overlay.
    pub feed: Option<Rect>,
    /// Whether the kill-feed layout has been checked against real matches. Unchecked layouts
    /// keep automatic clips off until the user turns them on for that game.
    pub feed_verified: bool,
}

/// Siege's kill feed (as `autoclip::SIEGE_FEED`, as a rect).
const SIEGE_FEED: Rect = (0.7406, 0.22, 0.2594, 0.26);
/// The common top-right kill feed.
const TOP_RIGHT: Rect = (0.70, 0.04, 0.29, 0.22);

pub const BUILTIN: &[Profile] = &[
    Profile {
        id: "siege",
        name: "Rainbow Six Siege",
        exes: &[
            "RainbowSix.exe",
            "RainbowSix_Vulkan.exe",
            "RainbowSix_BE.exe",
        ],
        anticheat: AntiCheat::BattlEye,
        method: Method::SiegeReplay,
        feed: Some(SIEGE_FEED),
        feed_verified: true,
    },
    Profile {
        id: "cs2",
        name: "Counter-Strike 2",
        exes: &["cs2.exe"],
        anticheat: AntiCheat::Vac,
        method: Method::Gsi,
        feed: Some(TOP_RIGHT),
        feed_verified: false,
    },
    Profile {
        id: "lol",
        name: "League of Legends",
        exes: &["League of Legends.exe"],
        anticheat: AntiCheat::Vanguard,
        method: Method::LiveClient,
        feed: None,
        feed_verified: false,
    },
    Profile {
        id: "valorant",
        name: "Valorant",
        exes: &["VALORANT-Win64-Shipping.exe"],
        anticheat: AntiCheat::Vanguard,
        method: Method::KillFeed,
        feed: Some(TOP_RIGHT),
        feed_verified: false,
    },
    Profile {
        id: "apex",
        name: "Apex Legends",
        exes: &["r5apex.exe", "r5apex_dx12.exe"],
        anticheat: AntiCheat::EasyAntiCheat,
        method: Method::KillFeed,
        feed: Some(TOP_RIGHT),
        feed_verified: false,
    },
    Profile {
        id: "fortnite",
        name: "Fortnite",
        exes: &["FortniteClient-Win64-Shipping.exe"],
        anticheat: AntiCheat::EasyAntiCheat,
        method: Method::KillFeed,
        feed: Some((0.01, 0.55, 0.30, 0.20)),
        feed_verified: false,
    },
    Profile {
        id: "overwatch",
        name: "Overwatch 2",
        exes: &["Overwatch.exe"],
        anticheat: AntiCheat::Other,
        method: Method::KillFeed,
        feed: Some(TOP_RIGHT),
        feed_verified: false,
    },
    Profile {
        id: "cod",
        name: "Call of Duty",
        exes: &["cod.exe", "cod24-cod.exe", "cod25-cod.exe"],
        anticheat: AntiCheat::Ricochet,
        method: Method::KillFeed,
        feed: Some((0.01, 0.55, 0.30, 0.20)),
        feed_verified: false,
    },
    Profile {
        id: "pubg",
        name: "PUBG: Battlegrounds",
        exes: &["TslGame.exe"],
        anticheat: AntiCheat::BattlEye,
        method: Method::KillFeed,
        feed: Some(TOP_RIGHT),
        feed_verified: false,
    },
    Profile {
        id: "marvel-rivals",
        name: "Marvel Rivals",
        exes: &["Marvel-Win64-Shipping.exe"],
        anticheat: AntiCheat::Other,
        method: Method::KillFeed,
        feed: Some(TOP_RIGHT),
        feed_verified: false,
    },
    Profile {
        id: "the-finals",
        name: "The Finals",
        exes: &["Discovery.exe"],
        anticheat: AntiCheat::EasyAntiCheat,
        method: Method::KillFeed,
        feed: Some(TOP_RIGHT),
        feed_verified: false,
    },
    Profile {
        id: "deadlock",
        name: "Deadlock",
        exes: &["deadlock.exe"],
        anticheat: AntiCheat::Vac,
        method: Method::Manual,
        feed: None,
        feed_verified: false,
    },
    Profile {
        id: "dota2",
        name: "Dota 2",
        exes: &["dota2.exe"],
        anticheat: AntiCheat::Vac,
        method: Method::Manual,
        feed: None,
        feed_verified: false,
    },
    Profile {
        id: "rocket-league",
        name: "Rocket League",
        exes: &["RocketLeague.exe"],
        anticheat: AntiCheat::EasyAntiCheat,
        method: Method::Manual,
        feed: None,
        feed_verified: false,
    },
    Profile {
        id: "xonotic",
        name: "Xonotic",
        exes: &["xonotic.exe", "xonotic-x64.exe", "xonotic-bench.exe"],
        anticheat: AntiCheat::None,
        method: Method::Manual,
        feed: None,
        feed_verified: false,
    },
];

/// The profile for games added by hand (`[game] processes`) and fullscreen apps.
pub const DEFAULT: Profile = Profile {
    id: "other",
    name: "Game",
    exes: &[],
    anticheat: AntiCheat::Other,
    method: Method::Manual,
    feed: None,
    feed_verified: false,
};

/// `[games.<id>]` in config.toml: per-game changes to a profile. Every key is optional.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Override {
    /// Display name (games added with "Add game…").
    pub name: Option<String>,
    /// More exe names for this game.
    #[serde(default)]
    pub exes: Vec<String>,
    /// Automatic clips on or off for this game.
    pub autoclip: Option<bool>,
    /// Strict safety for this game (loosening a Vanguard game's default warns).
    pub strict: Option<bool>,
    /// A `.cube` look for this game's grading and montages.
    pub look: Option<String>,
    /// Kill-feed region `[x, y, w, h]` as fractions of the frame.
    pub feed: Option<[f64; 4]>,
    /// HUD rectangles kept out of the grade's statistics, as fractions.
    #[serde(default)]
    pub hud: Vec<[f64; 4]>,
    /// The 9:16 crop's centre as a fraction of the width (ultrawide, off-centre HUDs).
    pub crop_center: Option<f64>,
}

/// A detected game with the user's overrides applied.
#[derive(Debug, Clone, PartialEq)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub anticheat: AntiCheat,
    pub method: Method,
    pub feed: Option<Rect>,
    pub autoclip: bool,
    pub strict: bool,
    pub look: Option<String>,
    pub hud: Vec<Rect>,
    pub crop_center: f64,
}

fn rect(r: [f64; 4]) -> Rect {
    (r[0], r[1], r[2], r[3])
}

/// Applies the overrides (if any) and the global safety setting to a profile.
pub fn resolve(p: &Profile, id: &str, o: Option<&Override>, global_strict: bool) -> Game {
    let o = o.cloned().unwrap_or_default();
    let feed = o.feed.map(rect).or(p.feed);
    // Kill-feed games clip automatically only once their layout is checked (or turned on).
    let auto_default = match p.method {
        Method::Manual => false,
        Method::KillFeed => p.feed_verified,
        _ => true,
    };
    Game {
        id: id.to_owned(),
        name: o.name.unwrap_or_else(|| p.name.to_owned()),
        anticheat: p.anticheat,
        method: p.method,
        feed,
        autoclip: o.autoclip.unwrap_or(auto_default) && p.method != Method::Manual,
        strict: o
            .strict
            .unwrap_or(global_strict || p.anticheat.strict_by_default()),
        look: o.look,
        hud: o.hud.into_iter().map(rect).collect(),
        crop_center: o.crop_center.unwrap_or(0.5).clamp(0.0, 1.0),
    }
}

/// Every exe name Retake records: the built-in database (if on), the user's games and
/// `[game] processes`.
pub fn all_exes(
    builtin: bool,
    extra: &[String],
    overrides: &BTreeMap<String, Override>,
) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if builtin {
        v.extend(
            BUILTIN
                .iter()
                .flat_map(|p| p.exes.iter().map(|e| (*e).to_owned())),
        );
    }
    v.extend(overrides.values().flat_map(|o| o.exes.iter().cloned()));
    v.extend(extra.iter().cloned());
    let mut seen = std::collections::HashSet::new();
    v.retain(|e| seen.insert(e.to_ascii_lowercase()));
    v
}

/// The game an exe name belongs to: a user game (`[games.<id>] exes`) first, then the
/// built-in database, then the default profile for `[game] processes`.
pub fn identify(
    exe: &str,
    builtin: bool,
    extra: &[String],
    overrides: &BTreeMap<String, Override>,
    global_strict: bool,
) -> Option<Game> {
    let eq = |a: &str| a.eq_ignore_ascii_case(exe);
    for (id, o) in overrides {
        if o.exes.iter().any(|e| eq(e)) {
            let p = BUILTIN.iter().find(|p| p.id == id).unwrap_or(&DEFAULT);
            return Some(resolve(p, id, Some(o), global_strict));
        }
    }
    if builtin && let Some(p) = BUILTIN.iter().find(|p| p.exes.iter().any(|e| eq(e))) {
        return Some(resolve(p, p.id, overrides.get(p.id), global_strict));
    }
    if extra.iter().any(|e| eq(e)) {
        let stem = exe.trim_end_matches(".exe").trim_end_matches(".EXE");
        let mut g = resolve(&DEFAULT, "other", overrides.get("other"), global_strict);
        g.name = stem.to_owned();
        return Some(g);
    }
    None
}

/// The first known game among running process names.
pub fn detect<'a>(
    running: impl IntoIterator<Item = &'a str>,
    builtin: bool,
    extra: &[String],
    overrides: &BTreeMap<String, Override>,
    global_strict: bool,
) -> Option<Game> {
    running
        .into_iter()
        .find_map(|n| identify(n, builtin, extra, overrides, global_strict))
}

/// A fractional rectangle as an even-sized pixel crop `(w, h, x, y)` of a `width × height`
/// frame.
pub fn crop_px(r: Rect, width: u32, height: u32) -> (u32, u32, u32, u32) {
    let (fw, fh) = (f64::from(width), f64::from(height));
    let x = ((r.0 * fw) as u32).min(width.saturating_sub(2));
    let y = ((r.1 * fh) as u32).min(height.saturating_sub(2));
    let w = (((r.2 * fw) as u32).min(width - x) / 2 * 2).max(2);
    let h = (((r.3 * fh) as u32).min(height - y) / 2 * 2).max(2);
    (w, h, x, y)
}

/// Everything detection needs from the config.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rules {
    pub builtin: bool,
    pub extra: Vec<String>,
    pub overrides: BTreeMap<String, Override>,
    pub strict: bool,
    pub any_fullscreen: bool,
}

impl Rules {
    pub fn from_config(c: &crate::config::Config) -> Rules {
        Rules {
            builtin: c.game.builtin,
            extra: c.game.processes.clone(),
            overrides: c.games.clone(),
            strict: c.safety.strict,
            any_fullscreen: c.game.any_fullscreen,
        }
    }

    pub fn exes(&self) -> Vec<String> {
        all_exes(self.builtin, &self.extra, &self.overrides)
    }

    pub fn identify(&self, exe: &str) -> Option<Game> {
        identify(exe, self.builtin, &self.extra, &self.overrides, self.strict)
    }

    /// A built-in game by id, with the user's changes.
    pub fn builtin_game(&self, id: &str) -> Option<Game> {
        BUILTIN
            .iter()
            .find(|p| p.id == id)
            .map(|p| resolve(p, id, self.overrides.get(id), self.strict))
    }

    /// A game by profile id: built-in or added by the user.
    pub fn game_by_id(&self, id: &str) -> Option<Game> {
        self.builtin_game(id).or_else(|| {
            self.overrides
                .get(id)
                .map(|o| resolve(&DEFAULT, id, Some(o), self.strict))
        })
    }

    /// The default profile for a fullscreen app.
    pub fn fullscreen_game(&self, exe: &str) -> Game {
        let mut g = resolve(&DEFAULT, "other", self.overrides.get("other"), self.strict);
        g.name = exe.rsplit_once('.').map_or(exe, |(s, _)| s).to_owned();
        g
    }
}

/// Processes that are never games, even fullscreen (for "Record any fullscreen app").
const NOT_GAMES: &[&str] = &[
    "explorer.exe",
    "chrome.exe",
    "msedge.exe",
    "firefox.exe",
    "opera.exe",
    "brave.exe",
    "vivaldi.exe",
    "vlc.exe",
    "mpc-hc64.exe",
    "Video.UI.exe",
    "Microsoft.Media.Player.exe",
    "PowerPoint.exe",
    "POWERPNT.EXE",
    "ApplicationFrameHost.exe",
    "retake.exe",
    "LockApp.exe",
    "SearchHost.exe",
    "StartMenuExperienceHost.exe",
    "ShellExperienceHost.exe",
    "steamwebhelper.exe",
    "Discord.exe",
];

/// Whether a fullscreen window of this process counts as a game for "Record any fullscreen
/// app".
pub fn fullscreen_counts(exe: &str) -> bool {
    !NOT_GAMES.iter().any(|n| n.eq_ignore_ascii_case(exe))
}

/// Whether a window rectangle covers a whole monitor (borderless or exclusive fullscreen).
pub fn covers_monitor(win: (i32, i32, i32, i32), mon: (i32, i32, i32, i32)) -> bool {
    win.0 <= mon.0 && win.1 <= mon.1 && win.2 >= mon.2 && win.3 >= mon.3
}

/// A profile id for a new game added from its exe name: lower case, letters, digits and '-'.
pub fn id_for_exe(exe: &str) -> String {
    let stem = exe
        .rsplit_once('.')
        .map_or(exe, |(s, _)| s)
        .to_ascii_lowercase();
    let mut id: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while id.contains("--") {
        id = id.replace("--", "-");
    }
    id.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> BTreeMap<String, Override> {
        BTreeMap::new()
    }

    #[test]
    fn exe_names_are_unique_across_profiles() {
        let mut seen = std::collections::HashSet::new();
        for p in BUILTIN {
            for e in p.exes {
                assert!(seen.insert(e.to_ascii_lowercase()), "{e} twice");
            }
            assert_eq!(id_for_exe(p.id), p.id, "ids are already normalised");
        }
    }

    #[test]
    fn known_games_are_detected_with_their_method() {
        let g = detect(["svchost.exe", "cs2.exe"], true, &[], &none(), false).unwrap();
        assert_eq!((g.id.as_str(), g.method), ("cs2", Method::Gsi));
        assert!(g.autoclip);
        let s = detect(["rainbowsix.exe"], true, &[], &none(), false).unwrap();
        assert_eq!(s.id, "siege");
        assert!(detect(["cs2.exe"], false, &[], &none(), false).is_none());
    }

    #[test]
    fn vanguard_games_are_strict_unless_loosened() {
        let v = detect(["VALORANT-Win64-Shipping.exe"], true, &[], &none(), false).unwrap();
        assert!(v.strict);
        // Unverified kill-feed layout: automatic clips stay off until turned on.
        assert!(!v.autoclip);
        let mut o = none();
        o.insert(
            "valorant".into(),
            Override {
                strict: Some(false),
                autoclip: Some(true),
                ..Default::default()
            },
        );
        let v = detect(["VALORANT-Win64-Shipping.exe"], true, &[], &o, false).unwrap();
        assert!(!v.strict && v.autoclip);
        let c = detect(["cs2.exe"], true, &[], &none(), false).unwrap();
        assert!(!c.strict);
        assert!(
            detect(["cs2.exe"], true, &[], &none(), true)
                .unwrap()
                .strict
        );
    }

    #[test]
    fn user_games_and_extra_processes() {
        let mut o = none();
        o.insert(
            "mygame".into(),
            Override {
                name: Some("My Game".into()),
                exes: vec!["MyGame-Win64.exe".into()],
                feed: Some([0.1, 0.1, 0.2, 0.2]),
                ..Default::default()
            },
        );
        let g = detect(["mygame-win64.exe"], true, &[], &o, false).unwrap();
        assert_eq!((g.id.as_str(), g.name.as_str()), ("mygame", "My Game"));
        assert_eq!(g.feed, Some((0.1, 0.1, 0.2, 0.2)));
        assert_eq!(g.method, Method::Manual);
        let extra = vec!["Indie.exe".to_owned()];
        let i = detect(["Indie.exe"], true, &extra, &o, false).unwrap();
        assert_eq!((i.id.as_str(), i.name.as_str()), ("other", "Indie"));
        let all = all_exes(true, &extra, &o);
        assert!(all.iter().any(|e| e == "MyGame-Win64.exe"));
        assert!(all.iter().any(|e| e == "Indie.exe"));
        assert!(all.iter().any(|e| e == "cs2.exe"));
        let dup = all_exes(true, &["CS2.EXE".to_owned()], &none());
        assert_eq!(
            dup.iter()
                .filter(|e| e.eq_ignore_ascii_case("cs2.exe"))
                .count(),
            1
        );
    }

    #[test]
    fn crops_stay_inside_the_frame() {
        assert_eq!(crop_px(TOP_RIGHT, 1920, 1080), (556, 236, 1344, 43));
        assert_eq!(crop_px((0.9, 0.9, 0.5, 0.5), 100, 100), (10, 10, 90, 90));
        // Siege's rect gives the same crop as autoclip::feed_crop on 16:9.
        let (w, h, x, y) = crop_px(SIEGE_FEED, 1920, 1080);
        let (sw, sh, sx, sy) = crate::autoclip::feed_crop(1920, 1080);
        assert!(w.abs_diff(sw) <= 2 && h.abs_diff(sh) <= 2 && x.abs_diff(sx) <= 2 && y == sy);
    }

    #[test]
    fn fullscreen_rules() {
        assert!(!fullscreen_counts("Explorer.EXE"));
        assert!(!fullscreen_counts("chrome.exe"));
        assert!(fullscreen_counts("SomeGame.exe"));
        let mon = (1920, 0, 3840, 1080);
        assert!(covers_monitor((1920, 0, 3840, 1080), mon));
        assert!(covers_monitor((1912, -8, 3848, 1088), mon));
        assert!(
            !covers_monitor((1920, 0, 3840, 1040), mon),
            "maximised with a taskbar"
        );
        assert_eq!(id_for_exe("My Game_v2.exe"), "my-game-v2");
    }
}
