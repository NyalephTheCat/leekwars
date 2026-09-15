//! A bulb's turn runs on what is left of its *owner's* per-turn operation
//! budget (#374).
//!
//! `BulbAI.runIA` calls `mAIFunction.run(mOwnerAI, …)`, so the operations are
//! charged to the owner's `AI` — whose counter only the owner's own
//! `EntityAI.runTurn` resets. Summoning therefore cannot buy a turn a second
//! `AI.MAX_OPERATIONS`, which is exactly what a bulb run used to be handed
//! here. The last test guards the other half of the change: the narrowed
//! budget is armed per run and is no part of the codegen key, so a budget that
//! shrinks every turn must not split the one-module-per-AI cache of #112 (see
//! `compile_once.rs`).

use std::collections::HashMap;
use std::sync::Arc;

use leek_backend_native::{jit_compiles, reset_jit_compiles};
use leek_game_runtime::actions::{AI_ERROR, LEEK_TURN};
use leek_generator::fight_options;
use leek_generator::official::{
    Area, BulbTemplate, ChipSpec, EffectModifiers, EffectParams, EffectTargets, EffectType,
    Fighter, STAT_AGILITY, STAT_FREQUENCY, STAT_LIFE, STAT_MP, STAT_RESISTANCE, STAT_STRENGTH,
    STAT_TP, State, Stats, WeaponSpec, run_official_fight,
};
use leek_hir::HirFile;
use leek_parser::{
    ParseFeatures,
    ast::{AstNode, SourceFile},
    parse_with_features,
};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

/// The synthetic summon chip and the bulb template it spawns — the corpus
/// harness' shapes (`official-fight`'s chip 1048 / bulb 1001), trimmed to what
/// `summonEntity` reads.
const SPAWN_CHIP: i32 = 1048;
const BULB_TEMPLATE: i32 = 1001;

/// The owner's per-turn budget. Small enough that the greedy AI below exhausts
/// it in milliseconds, wide enough for the summon scan.
const BUDGET: u64 = 200_000;

/// The bulb's fid: the two leeks take 0 and 1, and `createSummon` appends.
const BULB: i64 = 2;

fn compile(src: &str) -> Arc<HirFile> {
    let source = SourceId::new(1).unwrap();
    let parsed = parse_with_features(
        &format!("// @version: 4\n{src}\n"),
        source,
        Version::V4,
        ParseFeatures::default(),
    );
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
    Arc::new(leek_hir::lower_file_versioned(&sf, source, 4).0)
}

/// Summon a bulb on turn 1 and give it a cheap bounded AI. The target cell is
/// found by walking outwards from the owner's own cell (the map is generated
/// from the seed, so no fixed cell would do); a failed `summon` costs neither
/// TP nor cooldown, so the scan is free but for its operations.
const SUMMON: &str = r"
function bulbAi() {
    var s = 0
    for (var i = 0; i < 20; i++) {
        s = s + i
    }
    return s
}
if (getTurn() == 1) {
    var here = getCell()
    for (var d = 1; d <= 40; d++) {
        if (summon(1048, here - d, bulbAi) > 0) {
            break
        }
        if (summon(1048, here + d, bulbAi) > 0) {
            break
        }
    }
}
";

/// [`SUMMON`], then a loop far longer than the budget: the owner ends every
/// turn on `TOO_MUCH_OPERATIONS`, leaving its bulb nothing. Bounded rather
/// than `while (true)` so a broken budget makes the test slow, not eternal.
fn greedy_owner() -> String {
    format!(
        "{SUMMON}
var burn = 0
for (var k = 0; k < 10000000; k++) {{
    burn = burn + k
}}
"
    )
}

/// [`SUMMON`], then a loop that spends about half of [`BUDGET`] (measured at
/// ~100k operations for these 20k iterations, five per pass) — every turn, so
/// the owner's counter has to be *reset* per turn for this to stay under the
/// budget, not merely deducted from the bulb's.
fn half_spent_owner() -> String {
    format!(
        "{SUMMON}
var burn = 0
for (var k = 0; k < 20000; k++) {{
    burn = burn + k
}}
"
    )
}

/// A pistol (37) with no effects — these leeks never attack.
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
        passive_effects: Vec::new(),
        forgotten: false,
    }
}

/// `TYPE_SUMMON` chip 1048: 2 TP, range 1–8 free launch, no cooldown (only
/// one summon is ever cast, but a cooldown would also survive the fight).
fn spawn_chip() -> ChipSpec {
    ChipSpec {
        id: SPAWN_CHIP,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Summon,
            value1: f64::from(BULB_TEMPLATE),
            value2: 0.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// A bulb with no chips: it only ever runs its AI function.
fn bulb_template() -> BulbTemplate {
    BulbTemplate {
        id: BULB_TEMPLATE,
        name: "test_bulb".to_string(),
        life: (100, 400),
        strength: (0, 0),
        wisdom: (0, 0),
        agility: (0, 0),
        resistance: (0, 0),
        science: (0, 0),
        magic: (0, 0),
        tp: (4, 8),
        mp: (3, 6),
        chips: Vec::new(),
        states: Vec::new(),
        zone: 0,
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
    f.chips.insert(SPAWN_CHIP);
    f
}

/// A 1v1 where fid 0 runs `owner_ai` (and fid 1 idles), `BUDGET` operations
/// per turn.
fn run(owner_ai: &str) -> serde_json::Value {
    let mut state = State::new(7);
    state.add_entity(0, leek(1, "Owner", 100));
    state.add_entity(1, leek(2, "Foe", 200));
    state.weapon_specs.insert(37, pistol());
    state.chip_specs.insert(SPAWN_CHIP, spawn_chip());
    state.bulb_templates.insert(BULB_TEMPLATE, bulb_template());

    let ais: HashMap<usize, Arc<HirFile>> = HashMap::from([(0, compile(owner_ai))]);
    run_official_fight(state, &ais, &[100, 200], &fight_options(4, false, BUDGET))
}

/// How many `[code, entity, …]` actions the fight logged for `entity`.
fn count(outcome: &serde_json::Value, code: i64, entity: i64) -> usize {
    outcome["fight"]["actions"]
        .as_array()
        .expect("actions array")
        .iter()
        .filter(|a| {
            a.get(0) == Some(&serde_json::json!(code))
                && a.get(1) == Some(&serde_json::json!(entity))
        })
        .count()
}

/// The scaffolding itself: the owner really does summon a bulb, and the bulb
/// really does take turns. Every other test here reads those two counts.
#[test]
fn the_owner_summons_a_bulb_that_takes_turns() {
    let outcome = run(SUMMON);

    assert_eq!(
        outcome["fight"]["leeks"]
            .as_array()
            .expect("leeks array")
            .len(),
        3,
        "two leeks and the summoned bulb"
    );
    assert!(
        count(&outcome, LEEK_TURN, BULB) > 10,
        "the bulb takes a turn per round from the one it was summoned in"
    );
}

/// An owner that spends its whole turn leaves its bulb none of it: the bulb's
/// run errors at its first charged operation, on every turn. Before this
/// change the bulb was armed with a fresh `AI.MAX_OPERATIONS` and its cheap
/// loop ran to completion — the owner's turn got two budgets.
#[test]
fn a_bulb_gets_nothing_after_its_owner_spends_the_turn() {
    let outcome = run(&greedy_owner());

    let turns = count(&outcome, LEEK_TURN, BULB);
    assert!(turns > 10, "the bulb took {turns} turns");
    assert_eq!(
        count(&outcome, AI_ERROR, BULB),
        turns,
        "the bulb's budget is its owner's remainder, which is empty every turn"
    );
}

/// The remainder is a real budget and it comes back every turn: an owner that
/// spends half of it, on all 64 turns, still leaves its bulb enough to finish.
/// Deducting what the owner spends without resetting the count per turn would
/// starve the bulb from the second turn on (measured: 63 of its 64 turns).
#[test]
fn a_bulb_keeps_what_its_owner_left() {
    let outcome = run(&half_spent_owner());

    assert!(count(&outcome, LEEK_TURN, BULB) > 10);
    assert_eq!(
        count(&outcome, AI_ERROR, 0),
        0,
        "half the budget a turn is within the owner's own means"
    );
    assert_eq!(
        count(&outcome, AI_ERROR, BULB),
        0,
        "and the other half is still there for its bulb"
    );
}

/// The per-turn budget is armed by `run_call`, not compiled in: narrowing it
/// for every bulb turn must not add a compile. The owner's module is built
/// once for the whole fight, bulb turns included (#112).
#[test]
fn a_bulb_turn_reuses_its_owners_module() {
    reset_jit_compiles();
    let outcome = run(SUMMON);

    assert!(
        count(&outcome, LEEK_TURN, BULB) > 10,
        "the bulb ran on many turns, against a budget narrowed by whatever \
         its owner had spent in each"
    );
    assert_eq!(
        jit_compiles(),
        1,
        "one module for the owning AI, and none for its bulb's turns"
    );
}
