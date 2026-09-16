//! A fight compiles each AI once, not once per turn (#37 / GAME-03).
//!
//! `leek_backend_native::jit_compiles()` counts Cranelift modules built on this
//! thread, so the claim is measured rather than asserted: before this change a
//! two-entity fight over N turns paid 2×N full compiles (plus one per hook, and
//! one per bulb turn); it now pays one per AI, and one more per AI for the hook
//! roots the official runner force-compiles.
//!
//! The other half of the tests is the semantics the reuse must not disturb:
//! each turn still starts from cleared globals and a reseeded PRNG, and an AI
//! outside the native subset still errors on *every* turn rather than once.

use std::collections::HashMap;
use std::sync::Arc;

use leek_backend_native::{jit_compiles, reset_jit_compiles};
use leek_generator::official::{
    Area, Fighter, STAT_AGILITY, STAT_FREQUENCY, STAT_LIFE, STAT_MP, STAT_RESISTANCE,
    STAT_STRENGTH, STAT_TP, State, Stats, WeaponSpec, run_official_fight,
};
use leek_generator::{
    AiError, DEFAULT_MAX_OPS_PER_TURN, Entity, Fight, FightRef, fight_options, run_fight,
    run_fight_release, shared,
};
use leek_hir::HirFile;
use leek_parser::{
    ast::{AstNode, SourceFile},
    parse_with_features,
};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

fn compile(src: &str) -> HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = parse_with_features(
        &format!("// @version: 4\n{src}\n"),
        source,
        Version::V4,
        leek_parser::ParseFeatures::default(),
    );
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
    leek_hir::lower_file_versioned(&sf, source, 4).0
}

fn arena() -> FightRef {
    shared(
        Fight::new(10, 10, 1)
            .with_entity(Entity::new(1, "Bot", 0, 0))
            .with_entity(Entity::new(2, "Foe", 33, 1)),
    )
}

const TURNS: u32 = 20;

#[test]
fn a_fight_compiles_each_ai_once() {
    let f = arena();
    let ais: HashMap<i64, Arc<HirFile>> = HashMap::from([
        (1, Arc::new(compile("say(\"bot\" + getTurn())"))),
        (2, Arc::new(compile("say(\"foe\" + getTurn())"))),
    ]);

    reset_jit_compiles();
    let outcome = run_fight_release(&f, &ais, TURNS, 4, false, DEFAULT_MAX_OPS_PER_TURN);

    assert_eq!(outcome.turns, TURNS);
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    // Both AIs ran every turn (2 × TURNS runs), off two compiled modules.
    assert_eq!(f.borrow().log().len(), 2 * TURNS as usize);
    assert_eq!(
        jit_compiles(),
        2,
        "one module per AI for the whole fight, not one per AI per turn"
    );
}

/// A mirror match shares one `Arc<HirFile>` between both entities, so it shares
/// one module too — the cache is keyed on the HIR's identity, not on the entity.
#[test]
fn a_mirror_fight_compiles_one_module() {
    let f = arena();
    let ai = Arc::new(compile("say(\"hi\" + getTurn())"));
    let ais: HashMap<i64, Arc<HirFile>> =
        HashMap::from([(1, Arc::clone(&ai)), (2, Arc::clone(&ai))]);

    reset_jit_compiles();
    run_fight_release(&f, &ais, TURNS, 4, false, DEFAULT_MAX_OPS_PER_TURN);

    assert_eq!(jit_compiles(), 1);
}

/// The official runner (`Fight.startFight`) compiles a turn module per AI plus
/// a hook module per AI per hook it defines — `hook_roots` changes the
/// generated code, so it is part of the cache key and gets its own module.
/// Before this change it was one compile per entity per *turn* on top of those.
#[test]
fn an_official_fight_compiles_one_module_per_ai_and_hook() {
    let mut state = State::new(1);
    state.add_entity(0, leek(1, "A", 100));
    state.add_entity(1, leek(2, "B", 200));
    state.weapon_specs.insert(37, pistol());

    let mut ais: HashMap<usize, Arc<HirFile>> = HashMap::new();
    // FID 0 defines both hooks; FID 1 defines none.
    ais.insert(
        0,
        Arc::new(compile(
            "function beforeFight() { return 0; }\n\
             function afterFight() { return 0; }\n\
             return 0;",
        )),
    );
    ais.insert(1, Arc::new(compile("return 0;")));

    reset_jit_compiles();
    let outcome = run_official_fight(
        state,
        &ais,
        &[100, 200],
        &fight_options(4, false, DEFAULT_MAX_OPS_PER_TURN),
    );

    // The fight really ran to the turn cap (both AIs idle), i.e. each AI was
    // executed many times off its one module.
    assert!(outcome["fight"]["leeks"].is_array());
    // fid 0: turn module + `beforeFight` module + `afterFight` module.
    // fid 1: turn module only.
    assert_eq!(jit_compiles(), 4);
}

/// Cross-turn isolation, through the turn loop this time: a reused module must
/// still start every turn with cleared globals and a reseeded PRNG, or a seeded
/// fight would stop being reproducible.
#[test]
fn a_reused_module_still_starts_each_turn_clean() {
    let f = arena();
    let mut ais: HashMap<i64, HirFile> = HashMap::new();
    ais.insert(
        1,
        compile(
            "global n if (n == null) { n = 0 } n++ say(\"\" + n + \":\" + randInt(0, 1000000))",
        ),
    );

    run_fight(&f, &ais, 3, 4, false, DEFAULT_MAX_OPS_PER_TURN);

    let log: Vec<String> = f.borrow().log().iter().map(|(_, m)| m.clone()).collect();
    assert_eq!(log.len(), 3);
    // Same message all three turns: the global restarts at null and the PRNG
    // restarts from the same seed on every turn.
    assert_eq!(log[0], log[1]);
    assert_eq!(log[1], log[2]);
    assert!(log[0].starts_with("1:"), "{log:?}");
}

/// An AI outside the native subset failed to compile on every turn, and so
/// logged one error per turn. The cache must keep that: it stores the failure
/// and hands back a clone each turn instead of compiling again.
#[test]
fn a_cached_compile_failure_is_reported_every_turn() {
    let f = arena();
    let mut ais: HashMap<i64, HirFile> = HashMap::new();
    // `2 ** b` with a non-constant exponent is outside the native subset.
    ais.insert(1, compile("var b = 3 return 2 ** b"));

    reset_jit_compiles();
    let outcome = run_fight(&f, &ais, 3, 4, false, DEFAULT_MAX_OPS_PER_TURN);

    assert_eq!(
        outcome.errors,
        (1..=3)
            .map(|turn| AiError {
                turn,
                entity: 1,
                error: "unsupported: integer ** with non-constant/large exponent".into(),
            })
            .collect::<Vec<_>>()
    );
    assert_eq!(
        jit_compiles(),
        0,
        "a failing AI is compiled once (and builds no module), not once per turn"
    );
}

// --- official-fight scaffolding -----------------------------------------

/// A pistol (37) with no effects — these leeks never attack.
fn pistol() -> WeaponSpec {
    WeaponSpec {
        id: 37,
        template: 1,
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
