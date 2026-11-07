//! CS2 Game State Integration: the config file that tells CS2 to post its state to Retake, and
//! turning those posts into the player's kills. Valve's official interface: nothing is read
//! from the game; the game sends what the file asks for.

use serde_json::Value;

/// Retake's GSI port on 127.0.0.1.
pub const PORT: u16 = 3417;
/// File name in CS2's `game/csgo/cfg` folder.
pub const CFG_NAME: &str = "gamestate_integration_retake.cfg";

/// The `.cfg` CS2 reads at start. `token` comes back in every post, so other local programs
/// can't feed Retake fake kills.
pub fn cfg_text(token: &str) -> String {
    format!(
        "\"Retake\"\n{{\n \"uri\" \"http://127.0.0.1:{PORT}/\"\n \"timeout\" \"5.0\"\n \"buffer\" \"0.1\"\n \"throttle\" \"0.1\"\n \"heartbeat\" \"10.0\"\n \"auth\"\n {{\n  \"token\" \"{token}\"\n }}\n \"data\"\n {{\n  \"provider\" \"1\"\n  \"map\" \"1\"\n  \"round\" \"1\"\n  \"player_id\" \"1\"\n  \"player_state\" \"1\"\n  \"player_match_stats\" \"1\"\n }}\n}}\n"
    )
}

/// Whether a post carries our token.
pub fn authorised(post: &Value, token: &str) -> bool {
    post.pointer("/auth/token").and_then(Value::as_str) == Some(token)
}

/// New kills found in one post: how many, and in which round (1-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kills {
    pub count: u32,
    pub round: u32,
}

/// Follows the player's kill count across posts.
#[derive(Debug, Default)]
pub struct Tracker {
    /// Map name and the player's match kills at the last post about the player.
    last: Option<(String, u64)>,
}

impl Tracker {
    /// The player's new kills in `post`, if any. Posts about a spectated player (the provider's
    /// steam id differs from the player's) are ignored; a new map or a lower count restarts.
    pub fn on_post(&mut self, post: &Value) -> Option<Kills> {
        let me = post.pointer("/provider/steamid").and_then(Value::as_str)?;
        let player = post.pointer("/player/steamid").and_then(Value::as_str)?;
        if me != player {
            return None;
        }
        let kills = post
            .pointer("/player/match_stats/kills")
            .and_then(Value::as_u64)?;
        let map = post
            .pointer("/map/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        // `map.round` counts finished rounds; the round being played is one more, unless the
        // post comes in the round's "over" phase (the kill belongs to the round just ended).
        let finished = post
            .pointer("/map/round")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        let over = post.pointer("/round/phase").and_then(Value::as_str) == Some("over");
        let round = if over { finished.max(1) } else { finished + 1 };
        let prev = self.last.replace((map.clone(), kills));
        match prev {
            Some((m, k)) if m == map && kills > k => Some(Kills {
                count: (kills - k) as u32,
                round,
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn post(me: &str, player: &str, kills: u64, round: u64, phase: &str) -> Value {
        json!({
            "provider": {"name": "Counter-Strike: Global Offensive", "appid": 730, "steamid": me},
            "map": {"name": "de_mirage", "phase": "live", "round": round},
            "round": {"phase": phase},
            "player": {"steamid": player, "name": "p", "match_stats": {"kills": kills, "deaths": 0}},
            "auth": {"token": "t0k"}
        })
    }

    #[test]
    fn kills_come_from_the_players_own_count() {
        let mut t = Tracker::default();
        assert_eq!(
            t.on_post(&post("1", "1", 0, 0, "live")),
            None,
            "first post is a baseline"
        );
        assert_eq!(
            t.on_post(&post("1", "1", 1, 0, "live")),
            Some(Kills { count: 1, round: 1 })
        );
        // Dead and spectating a teammate: their kills aren't ours.
        assert_eq!(t.on_post(&post("1", "2", 9, 0, "live")), None);
        // A double kill in one post, in round 4, then a kill counted as the round ends.
        assert_eq!(
            t.on_post(&post("1", "1", 3, 3, "live")),
            Some(Kills { count: 2, round: 4 })
        );
        assert_eq!(
            t.on_post(&post("1", "1", 4, 4, "over")),
            Some(Kills { count: 1, round: 4 })
        );
        // A new match (count drops): baseline again.
        assert_eq!(t.on_post(&post("1", "1", 0, 0, "live")), None);
        assert_eq!(
            t.on_post(&post("1", "1", 1, 0, "live")),
            Some(Kills { count: 1, round: 1 })
        );
    }

    #[test]
    fn cfg_points_at_retake_with_the_token() {
        let c = cfg_text("abc");
        assert!(c.contains("\"uri\" \"http://127.0.0.1:3417/\""));
        assert!(c.contains("\"token\" \"abc\""));
        assert!(c.contains("\"player_match_stats\" \"1\""));
        assert!(authorised(&post("1", "1", 0, 0, "live"), "t0k"));
        assert!(!authorised(&post("1", "1", 0, 0, "live"), "other"));
    }
}
