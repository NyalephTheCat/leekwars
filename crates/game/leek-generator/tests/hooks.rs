//! End-to-end: the `beforeFight()` / `afterFight()` lifecycle hooks,
//! `setLoadout()` and `getWinner()` driven through the official fight runner
//! (`run_official_fight`) and the native JIT — the pieces that can't be
//! exercised by the State-level unit tests because they need a real compiled
//! AI invoked as a hook.

use std::collections::HashMap;
use std::sync::Arc;

use leek_generator::NativeOptions;
use leek_generator::official::{
    Area, Fighter, STAT_AGILITY, STAT_FREQUENCY, STAT_LIFE, STAT_MP, STAT_RESISTANCE,
    STAT_STRENGTH, STAT_TP, State, Stats, WeaponSpec, run_official_fight,
};
use leek_hir::HirFile;
use leek_parser::{
    ast::{AstNode, SourceFile},
    parse,
};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn compile(src: &str) -> Arc<HirFile> {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(&format!("// @version: 4\n{src}\n"), source, Version::V4);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
    Arc::new(leek_hir::lower_file_versioned(&sf, source, 4).0)
}

/// A pistol (37) with no effects — the leeks here never attack, so only the
/// template's existence matters (`setWeapon`/loadout validity checks).
fn pistol() -> WeaponSpec {
    WeaponSpec {
        id: 37,
        cost: 3,
        min_range: 1,
        max_range: 7,
        launch_type: 1,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: Vec::new(),
        forgotten: false,
    }
}

fn leek(id: i64, name: &str, farmer: i64) -> Fighter {
    let mut stats = Stats::default();
    stats.set(STAT_LIFE, 500);
    stats.set(STAT_TP, 6);
    stats.set(STAT_MP, 7);
    stats.set(STAT_STRENGTH, 100);
    stats.set(STAT_AGILITY, 100);
    stats.set(STAT_FREQUENCY, 10);
    stats.set(STAT_RESISTANCE, 10);
    let mut f = Fighter::new(0, id, name.to_string(), 0, stats);
    f.level = 10;
    f.farmer = farmer;
    f.weapons = vec![37];
    f
}

/// Does any system-log entry for `farmer` carry `key` (4th element of the
/// `[fid, type, trace, key, params?]` array)?
fn logged(outcome: &serde_json::Value, farmer: i64, key: i64) -> bool {
    let groups = &outcome["logs"][farmer.to_string()];
    let Some(groups) = groups.as_object() else {
        return false;
    };
    groups.values().any(|list| {
        list.as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|e| e.as_array().and_then(|a| a.get(3)) == Some(&serde_json::json!(key)))
        })
    })
}

/// `beforeFight()` applies a loadout (reflected in the initial-state
/// snapshot), and `afterFight()` reads `getWinner()` and is gated against
/// combat actions — all dispatched through the JIT as real hook calls.
#[test]
fn lifecycle_hooks_run_through_the_jit() {
    // Team 0's leek switches to a 1000-life loadout in beforeFight, so it wins
    // the 64-turn idle stalemate on the life tiebreak (team index 0).
    let mut state = State::new(1);
    let mut a = leek(1, "A", 100);
    let mut loadout = leek_generator::official::FightLoadout {
        name: "power".into(),
        weapons: vec![37],
        ..Default::default()
    };
    loadout.stats.insert(STAT_LIFE, 1000);
    a.add_loadout(loadout);
    state.add_entity(0, a);
    state.add_entity(1, leek(2, "B", 200));
    state.weapon_specs.insert(37, pistol());
    state.set_restat_potions_available(100, 1);

    let mut ais: HashMap<usize, Arc<HirFile>> = HashMap::new();
    // FID 0: applies the loadout before the fight; after the fight, reads the
    // winner (a denied combat action proves the hook ran and gating is live).
    ais.insert(
        0,
        compile(
            "function beforeFight() { setLoadout(\"power\"); }\n\
             function afterFight() { useWeapon(0); if (getWinner() == 0) { setLoadout(\"late\"); } }\n\
             return 0;",
        ),
    );
    // FID 1: no hooks.
    ais.insert(1, compile("return 0;"));

    let opts = NativeOptions::release()
        .with_lang(4, false)
        .with_link_game(true);
    let outcome = run_official_fight(state, &ais, &[100], &opts).expect("fight runs");

    // beforeFight's setLoadout took effect before the snapshot: team-0 leek
    // shows the loadout's 1000 max life.
    let leeks = outcome["fight"]["leeks"].as_array().expect("leeks array");
    let a_snap = leeks
        .iter()
        .find(|l| l["id"] == serde_json::json!(0))
        .expect("snapshot for fid 0");
    assert_eq!(
        a_snap["life"],
        serde_json::json!(1000),
        "loadout life applied"
    );

    // Team 0 wins the life tiebreak (1000 vs 500).
    assert_eq!(outcome["winner"], serde_json::json!(0));

    // afterFight ran: the combat action was denied (ACTION_DENIED_IN_HOOK,
    // 1008) and `getWinner() == 0` was true, so the out-of-(beforeFight)-hook
    // setLoadout warned (SET_LOADOUT_OUT_OF_HOOK, 1007).
    assert!(logged(&outcome, 100, 1008), "useWeapon denied in hook");
    assert!(
        logged(&outcome, 100, 1007),
        "setLoadout out of beforeFight hook"
    );
}

/// A `setLoadout()` for an unknown loadout name warns `LOADOUT_NOT_FOUND`
/// (1006) and leaves the kit unchanged — exercised through a real beforeFight
/// hook.
#[test]
fn set_loadout_unknown_name_warns_through_hook() {
    let mut state = State::new(1);
    state.add_entity(0, leek(1, "A", 100));
    state.add_entity(1, leek(2, "B", 200));
    state.weapon_specs.insert(37, pistol());

    let mut ais: HashMap<usize, Arc<HirFile>> = HashMap::new();
    ais.insert(
        0,
        compile("function beforeFight() { setLoadout(\"ghost\"); }\nreturn 0;"),
    );
    ais.insert(1, compile("return 0;"));

    let opts = NativeOptions::release()
        .with_lang(4, false)
        .with_link_game(true);
    let outcome = run_official_fight(state, &ais, &[100], &opts).expect("fight runs");

    assert!(logged(&outcome, 100, 1006), "LOADOUT_NOT_FOUND warning");
    // Unchanged life: still the base 500.
    let leeks = outcome["fight"]["leeks"].as_array().expect("leeks array");
    let a_snap = leeks
        .iter()
        .find(|l| l["id"] == serde_json::json!(0))
        .expect("snapshot for fid 0");
    assert_eq!(a_snap["life"], serde_json::json!(500));
}
