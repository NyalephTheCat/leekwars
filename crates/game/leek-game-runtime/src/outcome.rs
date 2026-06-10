//! Official-format fight report — ports of `outcome/Outcome.java` and the
//! `leeks`/`map`/`dead` serialization in `action/Actions.java` /
//! `State.getDeadReport()`.
//!
//! The conformance harness diffs this JSON against the Java generator's
//! output byte-for-byte (modulo `execution_time`), so every field —
//! including upstream quirks like `height` being emitted from `getWidth()`
//! — is reproduced exactly.

use serde_json::{Map as JsonMap, Value, json};

use crate::actions::ActionLog;
use crate::map::Map;
use crate::state::{
    Fighter, STAT_AGILITY, STAT_FREQUENCY, STAT_MAGIC, STAT_RESISTANCE, STAT_SCIENCE,
    STAT_STRENGTH, STAT_WISDOM, Team,
};

/// One entity's snapshot in `fight.leeks` (`Actions.addEntity`, leek scope:
/// cosmetics default, never a summon).
///
/// Captured at `recordInitialState` time — current life/TP/MP, base + buff
/// stats, current cell.
#[must_use]
pub fn entity_snapshot(f: &Fighter) -> Value {
    json!({
        "id": f.fid,
        "level": f.level,
        "skin": 0,
        "hat": Value::Null, // getHat() > 0 ? hat : null — leeks default to 0
        "metal": false,
        "face": 0,
        "life": f.life,
        "strength": f.stat(STAT_STRENGTH),
        "wisdom": f.stat(STAT_WISDOM),
        "agility": f.stat(STAT_AGILITY),
        "resistance": f.stat(STAT_RESISTANCE),
        "frequency": f.stat(STAT_FREQUENCY),
        "science": f.stat(STAT_SCIENCE),
        "magic": f.stat(STAT_MAGIC),
        "tp": f.tp(),
        "mp": f.mp(),
        "team": f.team + 1,
        "name": f.name,
        "cellPos": f.cell.map_or(Value::Null, |c| json!(c)),
        "farmer": f.farmer,
        "type": 0, // Entity.TYPE_LEEK
        "orientation": -1,
        "summon": false,
    })
}

/// `fight.map` (`Actions.addMap`, generated-map branch: `id == 0`, not
/// custom). Obstacle cells emit their **size**; sub-cell markers
/// (`obstacle_size <= 0`) are skipped.
#[must_use]
pub fn map_json(map: &Map) -> Value {
    let mut obstacles = JsonMap::new();
    for cell in &map.cells {
        if !cell.walkable && cell.obstacle_size > 0 {
            obstacles.insert(cell.id.to_string(), json!(cell.obstacle_size));
        }
    }
    json!({
        "obstacles": obstacles,
        "type": map.map_type,
        "width": map.width,
        "height": map.width, // upstream emits getWidth() for height too
    })
}

/// `fight.dead` (`State.getDeadReport`) — real entity id → is-dead, walking
/// teams in order.
#[must_use]
pub fn dead_report(teams: &[Team], fighters: &[Fighter]) -> Value {
    let mut dead = JsonMap::new();
    for team in teams {
        for &fid in &team.fighters {
            let f = &fighters[fid];
            dead.insert(f.id.to_string(), json!(f.is_dead()));
        }
    }
    Value::Object(dead)
}

/// The full Outcome (`Outcome.toJson`).
///
/// `leeks` is the snapshot array captured at `recordInitialState` (initial
/// order); `farmers` lists farmer ids for the per-farmer `logs` objects
/// (debug logs themselves are not emitted yet). `execution_time` is written
/// as 0 — the conformance diff ignores it.
#[must_use]
pub fn build_outcome(
    leeks: &[Value],
    map: &Map,
    log: &ActionLog,
    teams: &[Team],
    fighters: &[Fighter],
    farmers: &[i64],
    winner: i32,
    duration: i32,
) -> Value {
    let mut logs = JsonMap::new();
    for farmer in farmers {
        logs.insert(farmer.to_string(), json!({}));
    }
    json!({
        "fight": {
            "leeks": leeks,
            "map": map_json(map),
            "actions": log.to_json(),
            "dead": dead_report(teams, fighters),
            "ops": log.ops,
        },
        "logs": logs,
        "winner": winner,
        "duration": duration,
        "analyze_time": 0,
        "compilation_time": 0,
        "execution_time": 0,
    })
}

#[cfg(test)]
mod tests {
    use super::{dead_report, entity_snapshot};
    use crate::state::{
        Fighter, STAT_AGILITY, STAT_LIFE, STAT_MP, STAT_STRENGTH, STAT_TP, Stats, Team,
    };
    use serde_json::json;

    fn harness_leek(fid: usize, id: i64, team: usize, cell: usize) -> Fighter {
        // The fight-harness default leek: level 10, 500 life, 100/50/100
        // str/wis/agi, 10 res, 10 freq, 6 TP, 7 MP.
        let mut stats = Stats::default();
        stats.set(STAT_LIFE, 500);
        stats.set(STAT_TP, 6);
        stats.set(STAT_MP, 7);
        stats.set(STAT_STRENGTH, 100);
        stats.set(crate::state::STAT_WISDOM, 50);
        stats.set(STAT_AGILITY, 100);
        stats.set(crate::state::STAT_RESISTANCE, 10);
        stats.set(crate::state::STAT_FREQUENCY, 10);
        let mut f = Fighter::new(fid, id, format!("AI_{id}"), team, stats);
        f.level = 10;
        f.cell = Some(cell);
        f
    }

    /// Snapshot matches the golden `fight.leeks[0]` from the Java oracle
    /// (chase_vs_chase seed 42).
    #[test]
    fn snapshot_matches_java_golden() {
        let f = harness_leek(0, 1, 0, 284);
        assert_eq!(
            entity_snapshot(&f),
            json!({
                "id": 0, "level": 10, "skin": 0, "hat": null, "metal": false,
                "face": 0, "life": 500, "strength": 100, "wisdom": 50,
                "agility": 100, "resistance": 10, "frequency": 10,
                "science": 0, "magic": 0, "tp": 6, "mp": 7, "team": 1,
                "name": "AI_1", "cellPos": 284, "farmer": 0, "type": 0,
                "orientation": -1, "summon": false,
            })
        );
    }

    /// Dead report keys are REAL entity ids (not fids), like
    /// `State.getDeadReport`.
    #[test]
    fn dead_report_keyed_by_real_id() {
        let fighters = [harness_leek(0, 1, 0, 10), harness_leek(1, 2, 1, 20)];
        let mut dead_leek = harness_leek(0, 1, 0, 10);
        dead_leek.life = 0;
        let fighters_dead = vec![dead_leek, fighters[1].clone()];
        let teams = vec![
            Team {
                id: 1,
                fighters: vec![0],
                ..Team::default()
            },
            Team {
                id: 2,
                fighters: vec![1],
                ..Team::default()
            },
        ];
        assert_eq!(
            dead_report(&teams, &fighters_dead),
            json!({"1": true, "2": false})
        );
    }
}
