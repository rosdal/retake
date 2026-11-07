//! League of Legends' Live Client Data API (`https://127.0.0.1:2999/liveclientdata/allgamedata`,
//! served by the game itself while a match runs): the player's champion kills.

use serde_json::Value;

pub const URL_HOST: &str = "127.0.0.1";
pub const URL_PORT: u16 = 2999;
pub const URL_PATH: &str = "/liveclientdata/allgamedata";

/// Kills further apart than this are separate plays (League's multi-kill timer is 10 s).
pub const MULTI_KILL_GAP: f64 = 10.0;

/// Follows the event list across polls.
#[derive(Debug, Default)]
pub struct Tracker {
    /// Highest event id handled so far.
    seen: Option<u64>,
}

impl Tracker {
    /// The player's new kills in one `allgamedata` response, as seconds before now (the
    /// game's clock minus the event's time). The first response only sets the baseline when
    /// Retake starts mid-match, so old kills aren't clipped twice.
    pub fn on_poll(&mut self, data: &Value) -> Vec<f64> {
        let names: Vec<&str> = ["summonerName", "riotId", "riotIdGameName"]
            .iter()
            .filter_map(|k| data.pointer(&format!("/activePlayer/{k}"))?.as_str())
            .filter(|n| !n.is_empty())
            .collect();
        let now = data
            .pointer("/gameData/gameTime")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let Some(events) = data.pointer("/events/Events").and_then(Value::as_array) else {
            return Vec::new();
        };
        let first = self.seen.is_none();
        let seen = self.seen;
        let mut out = Vec::new();
        for e in events {
            let Some(id) = e.get("EventID").and_then(Value::as_u64) else {
                continue;
            };
            if seen.is_some_and(|s| id <= s) {
                continue;
            }
            self.seen = Some(self.seen.map_or(id, |s| s.max(id)));
            if e.get("EventName").and_then(Value::as_str) != Some("ChampionKill") {
                continue;
            }
            let killer = e.get("KillerName").and_then(Value::as_str).unwrap_or("");
            let mine = names.iter().any(|n| {
                n.eq_ignore_ascii_case(killer)
                    || n.split('#')
                        .next()
                        .is_some_and(|g| g.eq_ignore_ascii_case(killer))
            });
            let at = e.get("EventTime").and_then(Value::as_f64).unwrap_or(now);
            // Started mid-match: only kills from the last few seconds are new.
            if mine && (!first || now - at < 5.0) {
                out.push((now - at).max(0.0));
            }
        }
        if self.seen.is_none() {
            // An empty list still counts as a baseline.
            self.seen = Some(0);
        }
        out
    }
}

/// Groups sorted kill times into plays: a kill within [`MULTI_KILL_GAP`] of the previous one
/// joins its play.
pub fn plays(times: &[f64]) -> Vec<Vec<f64>> {
    let mut out: Vec<Vec<f64>> = Vec::new();
    for &t in times {
        match out.last_mut() {
            Some(g) if t - g.last().copied().unwrap_or(t) <= MULTI_KILL_GAP => g.push(t),
            _ => out.push(vec![t]),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn data(now: f64, events: Value) -> Value {
        json!({
            "activePlayer": {"summonerName": "", "riotId": "Faker#KR1", "riotIdGameName": "Faker"},
            "events": {"Events": events},
            "gameData": {"gameTime": now}
        })
    }

    #[test]
    fn only_my_new_kills() {
        let mut t = Tracker::default();
        let start = json!([{"EventID": 0, "EventName": "GameStart", "EventTime": 0.03}]);
        assert!(t.on_poll(&data(60.0, start)).is_empty());
        let ev = json!([
            {"EventID": 0, "EventName": "GameStart", "EventTime": 0.03},
            {"EventID": 1, "EventName": "ChampionKill", "EventTime": 118.0, "KillerName": "Faker", "VictimName": "Zed"},
            {"EventID": 2, "EventName": "ChampionKill", "EventTime": 119.0, "KillerName": "Other", "VictimName": "Faker"},
            {"EventID": 3, "EventName": "Multikill", "EventTime": 119.5, "KillerName": "Faker", "KillStreak": 2},
            {"EventID": 4, "EventName": "ChampionKill", "EventTime": 119.5, "KillerName": "Faker#KR1", "VictimName": "Ahri"}
        ]);
        let k = t.on_poll(&data(120.0, ev.clone()));
        assert_eq!(k.len(), 2);
        assert!((k[0] - 2.0).abs() < 1e-9 && (k[1] - 0.5).abs() < 1e-9);
        assert!(t.on_poll(&data(121.0, ev)).is_empty(), "each event once");
    }

    #[test]
    fn starting_mid_match_skips_old_kills() {
        let mut t = Tracker::default();
        let ev = json!([
            {"EventID": 7, "EventName": "ChampionKill", "EventTime": 300.0, "KillerName": "Faker"},
            {"EventID": 8, "EventName": "ChampionKill", "EventTime": 598.0, "KillerName": "Faker"}
        ]);
        assert_eq!(t.on_poll(&data(600.0, ev)), vec![2.0]);
    }

    #[test]
    fn multi_kills_group() {
        assert_eq!(
            plays(&[10.0, 15.0, 24.0, 60.0]),
            vec![vec![10.0, 15.0, 24.0], vec![60.0]]
        );
        assert!(plays(&[]).is_empty());
    }
}
