//! Reading r6-dissect's JSON for a Siege Match Replay: who recorded it (the player), the teams
//! and each round's kills. Lenient about shape, because r6-dissect's output changes a little
//! between seasons: a match export (`{"rounds": [...]}`) or a single round both work, and an
//! event type may be a string or `{"name": ...}`.

use serde_json::Value;

use crate::autoclip::ReplayKill;

#[derive(Debug, Clone, PartialEq)]
pub struct Kill {
    pub killer: String,
    pub victim: String,
    pub headshot: bool,
    /// Seconds on the round clock, negated so later kills are larger (the clock counts down).
    pub at: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Round {
    pub number: usize,
    pub map: Option<String>,
    /// The recording player's name.
    pub me: Option<String>,
    /// Players on the other team (an ACE is all of them).
    pub enemies: usize,
    pub kills: Vec<Kill>,
}

impl Round {
    /// The round's kills for [`crate::autoclip::align`], marking the player's own.
    pub fn replay_kills(&self) -> Vec<ReplayKill> {
        self.kills
            .iter()
            .map(|k| ReplayKill {
                at: k.at,
                mine: self
                    .me
                    .as_deref()
                    .is_some_and(|me| me.eq_ignore_ascii_case(&k.killer)),
            })
            .collect()
    }
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

fn kind(v: &Value) -> Option<&str> {
    match v.get("type")? {
        Value::String(s) => Some(s),
        Value::Object(o) => o.get("name")?.as_str(),
        _ => None,
    }
}

fn round(v: &Value, number: usize) -> Round {
    let players = v
        .get("players")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let rec_id = v.get("recordingPlayerID").and_then(Value::as_u64);
    let me_player = players
        .iter()
        .find(|p| rec_id.is_some() && p.get("id").and_then(Value::as_u64) == rec_id);
    let me = me_player.and_then(|p| s(p, "username"));
    let my_team = me_player
        .and_then(|p| p.get("teamIndex"))
        .and_then(Value::as_u64);
    let enemies = match my_team {
        Some(t) => players
            .iter()
            .filter(|p| p.get("teamIndex").and_then(Value::as_u64) != Some(t))
            .count(),
        None => 5,
    };
    let map = v.get("map").and_then(|m| match m {
        Value::String(s) => Some(s.clone()),
        Value::Object(o) => o.get("name")?.as_str().map(str::to_owned),
        _ => None,
    });
    let kills = v
        .get("matchFeedback")
        .and_then(Value::as_array)
        .map(|events| {
            events
                .iter()
                .filter(|e| kind(e).is_some_and(|k| k.eq_ignore_ascii_case("kill")))
                .filter_map(|e| {
                    Some(Kill {
                        killer: s(e, "username")?,
                        victim: s(e, "target").unwrap_or_default(),
                        headshot: e.get("headshot").and_then(Value::as_bool).unwrap_or(false),
                        at: -e.get("timeInSeconds")?.as_f64()?,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Round {
        number,
        map,
        me,
        enemies: enemies.max(1),
        kills,
    }
}

/// Parses r6-dissect's output for a match (or one round).
pub fn parse(json: &str) -> Result<Vec<Round>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let rounds: Vec<Round> = match v.get("rounds").and_then(Value::as_array) {
        Some(rs) => rs
            .iter()
            .enumerate()
            .map(|(i, r)| round(r, i + 1))
            .collect(),
        None if v.get("matchFeedback").is_some() => vec![round(&v, 1)],
        None => return Err("no rounds in the replay output".into()),
    };
    if rounds.iter().all(|r| r.me.is_none()) {
        return Err("the replay doesn't say who recorded it".into());
    }
    Ok(rounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROUND: &str = r#"{
        "map": {"name": "CLUBHOUSE", "id": 1},
        "recordingPlayerID": 42,
        "players": [
            {"id": 42, "username": "Me", "teamIndex": 0},
            {"id": 2, "username": "Mate", "teamIndex": 0},
            {"id": 7, "username": "Foe1", "teamIndex": 1},
            {"id": 8, "username": "Foe2", "teamIndex": 1}
        ],
        "matchFeedback": [
            {"type": "Kill", "username": "Me", "target": "Foe1", "headshot": true, "time": "2:51", "timeInSeconds": 171},
            {"type": "Other", "message": "Defuser planted", "time": "1:00", "timeInSeconds": 60},
            {"type": {"name": "Kill", "id": 0}, "username": "Foe2", "target": "Mate", "headshot": false, "timeInSeconds": 150},
            {"type": "Kill", "username": "me", "target": "Foe2", "timeInSeconds": 148.5}
        ]
    }"#;

    #[test]
    fn single_round() {
        let r = parse(ROUND).unwrap();
        assert_eq!(r.len(), 1);
        let r = &r[0];
        assert_eq!(r.map.as_deref(), Some("CLUBHOUSE"));
        assert_eq!(r.me.as_deref(), Some("Me"));
        assert_eq!(r.enemies, 2);
        assert_eq!(r.kills.len(), 3);
        assert_eq!(r.kills[0].at, -171.0);
        assert!(r.kills[0].headshot);
        let mine: Vec<bool> = r.replay_kills().iter().map(|k| k.mine).collect();
        assert_eq!(mine, [true, false, true]);
    }

    #[test]
    fn match_export_numbers_rounds() {
        let m = format!("{{\"rounds\": [{ROUND}, {ROUND}]}}");
        let r = parse(&m).unwrap();
        assert_eq!(r.iter().map(|x| x.number).collect::<Vec<_>>(), [1, 2]);
    }

    #[test]
    fn errors_are_clear() {
        assert!(parse("{}").unwrap_err().contains("no rounds"));
        assert!(
            parse("{\"matchFeedback\": []}")
                .unwrap_err()
                .contains("who recorded")
        );
        assert!(parse("not json").is_err());
    }
}
