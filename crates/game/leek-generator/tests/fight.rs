//! End-to-end across all layers via native AIs: queries, map geometry,
//! movement/communication, the real weapon damage formula, and the turn loop.

use std::collections::HashMap;

use leek_game_runtime::{EffectKind, GameHost}; // `life`/… accessors on `Fight`
use leek_generator::{run_ai, run_fight, shared, ActiveEffect, Entity, Fight, FightRef};
use leek_hir::{lower_file_versioned, HirFile};
use leek_parser::{ast::AstNode, ast::SourceFile, parse};
use leek_span::SourceId;
use leek_syntax::{SyntaxNode, Version};

const WEAPON_PISTOL: i64 = 37; // 15–20 dmg, 3 TP, range 1–7
const WEAPON_M_LASER: i64 = 47; // 90–100 dmg, 8 TP, range 5–12
const CHIP_SPARK: i64 = 18; // 8–16 dmg, 3 TP, range 0–10
const CHIP_CURE: i64 = 4; // heal 35–43, 4 TP, range 0–5
const CHIP_ARMOR: i64 = 22; // +25 absolute shield (×resistance), 4 turns
const CHIP_PROTEIN: i64 = 8; // +80–100 strength buff (×science), 2 turns

fn compile(src: &str) -> HirFile {
    let source = SourceId::new(1).unwrap();
    let parsed = parse(&format!("// @version: 4\n{src}\n"), source, Version::V4);
    let sf = SourceFile::cast(SyntaxNode::new_root(parsed.green)).expect("parse");
    lower_file_versioned(&sf, source, 4).0
}

fn run(fight: &FightRef, src: &str) -> String {
    let hir = compile(src);
    run_ai(fight, &hir, 4, false).map_or_else(|e| format!("ERR: {e}"), |v| v.to_string())
}

/// Bot (id 1) at cell 0 = (0,0); Foe (id 2) at cell 33 = (3,3) → distance 6.
fn arena(bot: Entity, foe: Entity) -> FightRef {
    shared(Fight::new(10, 10, 1).with_entity(bot).with_entity(foe))
}

#[test]
fn layer_queries_and_map() {
    let f = arena(Entity::new(1, "Bot", 0, 0), Entity::new(2, "Foe", 33, 1));
    assert_eq!(run(&f, "return getEntity()"), "1");
    assert_eq!(run(&f, "return getCellX(33)"), "3");
    assert_eq!(run(&f, "return getCellDistance(getCell(), getCell(2))"), "6");
    assert_eq!(run(&f, "return isOnSameLine(0, 33)"), "true");
}

#[test]
fn layer_actions_move_and_say() {
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_points(5, 10),
        Entity::new(2, "Foe", 33, 1),
    );
    assert_eq!(run(&f, "moveTowardCell(33, 3) return getCellDistance(getCell(), 33)"), "3");
    run(&f, "say(\"engaging\")");
    assert_eq!(f.borrow().log().last().map(|(_, m)| m.clone()), Some("engaging".to_string()));
}

#[test]
fn weapon_damage_formula() {
    // strength 0: pistol deals 15–20 → foe (100 life) ends at 80–85.
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_PISTOL),
        Entity::new(2, "Foe", 33, 1),
    );
    assert_eq!(run(&f, "return useWeapon(2)"), "1"); // USE_SUCCESS
    let life = f.borrow().life(2).unwrap();
    assert!((80..=85).contains(&life), "str-0 pistol left {life}, expected 80..=85");

    // strength 100 doubles the roll: 30–40 → foe ends at 60–70.
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_PISTOL).with_strength(100),
        Entity::new(2, "Foe", 33, 1),
    );
    run(&f, "useWeapon(2)");
    let life = f.borrow().life(2).unwrap();
    assert!((60..=70).contains(&life), "str-100 pistol left {life}, expected 60..=70");
}

#[test]
fn shields_reduce_damage() {
    // Absolute shield 10 subtracts flat from the 15–20 roll → 5–10 dealt.
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_PISTOL),
        Entity::new(2, "Foe", 33, 1).with_shields(0, 10),
    );
    run(&f, "useWeapon(2)");
    let life = f.borrow().life(2).unwrap();
    assert!((90..=95).contains(&life), "shielded foe left {life}, expected 90..=95");
}

#[test]
fn use_rules_range_and_tp() {
    // m_laser min range 5, but Foe is 6 away → in range; move it close first.
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_M_LASER).with_points(5, 12),
        Entity::new(2, "Foe", 11, 1), // (1,1) → distance 2 < min range 5
    );
    assert_eq!(run(&f, "return useWeapon(2)"), "-1"); // USE_INVALID_TARGET (out of range)

    // Not enough TP: pistol costs 3, give the bot 2.
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_PISTOL).with_points(5, 2),
        Entity::new(2, "Foe", 33, 1),
    );
    assert_eq!(run(&f, "return useWeapon(2)"), "-2"); // USE_NOT_ENOUGH_TP

    // No weapon equipped.
    let f = arena(Entity::new(1, "Bot", 0, 0), Entity::new(2, "Foe", 33, 1));
    assert_eq!(run(&f, "return useWeapon(2)"), "0"); // USE_FAILED
}

#[test]
fn stat_getters() {
    let bot = Entity::new(1, "Bot", 0, 0).with_magic_stats(40, 30, 20, 50);
    let f = arena(bot, Entity::new(2, "Foe", 33, 1));
    assert_eq!(run(&f, "return getWisdom()"), "40");
    assert_eq!(run(&f, "return getResistance()"), "30");
    assert_eq!(run(&f, "return getScience()"), "20");
    assert_eq!(run(&f, "return getMagic()"), "50");
}

#[test]
fn chip_damage_and_heal() {
    // spark (8–16, range 0–10) hits the foe at distance 6.
    let f = arena(
        Entity::new(1, "Bot", 0, 0),
        Entity::new(2, "Foe", 33, 1),
    );
    assert_eq!(run(&f, &format!("return useChip({CHIP_SPARK}, 2)")), "1"); // USE_SUCCESS
    let life = f.borrow().life(2).unwrap();
    assert!((84..=92).contains(&life), "spark left {life}, expected 84..=92");

    // cure heals self (wisdom 100 → ×2 on a 35–43 roll = 70–86).
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_life(150).with_magic_stats(100, 0, 0, 0),
        Entity::new(2, "Foe", 33, 1),
    );
    f.borrow_mut().deal_damage(1, 100); // wound Bot: 150 → 50
    run(&f, &format!("useChip({CHIP_CURE}, getEntity())"));
    let life = f.borrow().life(1).unwrap();
    assert!((120..=136).contains(&life), "cure left {life}, expected 120..=136");
}

#[test]
fn chip_shield_and_buff() {
    // armor: +25 absolute shield ×(1+resistance/100). resistance 100 → +50.
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_magic_stats(0, 100, 0, 0),
        Entity::new(2, "Foe", 33, 1),
    );
    run(&f, &format!("useChip({CHIP_ARMOR}, getEntity())"));
    assert_eq!(run(&f, "return getAbsoluteShield()"), "50");

    // protein: +80–100 strength buff ×(1+science/100). science 0 → +80..100.
    let f = arena(
        Entity::new(1, "Bot", 0, 0),
        Entity::new(2, "Foe", 33, 1),
    );
    run(&f, &format!("useChip({CHIP_PROTEIN}, getEntity())"));
    let str_after = run(&f, "return getStrength()").parse::<i64>().unwrap();
    assert!((80..=100).contains(&str_after), "protein gave strength {str_after}, expected 80..=100");
}

#[test]
fn poison_over_time() {
    // A poison effect deals its value each turn for `turns` turns.
    let mut foe = Entity::new(2, "Foe", 33, 1).with_life(25);
    foe.effects.push(ActiveEffect { kind: EffectKind::Poison, value: 10, turns: 3 });
    let f = arena(Entity::new(1, "Bot", 0, 0), foe);
    // Neither has an AI; the turn loop still ticks poison on the foe's turn.
    let outcome = run_fight(&f, &std::collections::HashMap::new(), 10, 4, false).expect("runs");
    assert!(f.borrow().life(2).unwrap() <= 0, "poison should kill the foe");
    assert_eq!(outcome.winner_team, Some(0));
}

#[test]
fn critical_hits() {
    // Agility 1000 → crit chance 100%: the roll is always < 1.0. Crit applies
    // the ×1.3 factor, so the foe ends lower than a normal 15–20 pistol hit.
    let mut bot = Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_PISTOL);
    bot.agility = 1000;
    let f = arena(bot, Entity::new(2, "Foe", 33, 1));
    assert_eq!(run(&f, "return useWeapon(2)"), "2"); // USE_CRITICAL
    let life = f.borrow().life(2).unwrap();
    assert!((74..=81).contains(&life), "crit pistol left {life}, expected 74..=81");
}

#[test]
fn life_steal_and_erosion() {
    // Wisdom 1000 → steal = full damage dealt; the bot (wounded to 50) heals.
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_PISTOL).with_life(150).with_magic_stats(1000, 0, 0, 0),
        Entity::new(2, "Foe", 33, 1),
    );
    f.borrow_mut().deal_damage(1, 100); // Bot → 50
    run(&f, "useWeapon(2)");
    let bot = f.borrow().life(1).unwrap();
    assert!((65..=70).contains(&bot), "life-steal left bot at {bot}, expected 65..=70");
    // Erosion: 15–20 damage × 0.05 rounds to 1 → foe max life 100 → 99.
    assert_eq!(run(&f, "return getTotalLife(2)"), "99");
}

#[test]
fn damage_return() {
    // Foe reflects 50% of incoming damage (pre-shield 15–20 → 8–10 back).
    let bot = Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_PISTOL).with_life(150);
    let mut foe = Entity::new(2, "Foe", 33, 1);
    foe.damage_return = 50;
    let f = arena(bot, foe);
    run(&f, "useWeapon(2)");
    let bot = f.borrow().life(1).unwrap();
    assert!((140..=143).contains(&bot), "return damage left bot at {bot}, expected 140..=143");
}

#[test]
fn buff_any_stat() {
    // A buff effect raises the effective stat; here agility +30.
    let mut bot = Entity::new(1, "Bot", 0, 0);
    bot.effects.push(ActiveEffect { kind: EffectKind::Buff(leek_game_runtime::Stat::Agility), value: 30, turns: 5 });
    let f = arena(bot, Entity::new(2, "Foe", 33, 1));
    assert_eq!(run(&f, "return getAgility()"), "30");
}

const CHIP_VENOM: i64 = 97;
const CHIP_REGENERATION: i64 = 35;
const CHIP_FRACTURE: i64 = 106;
const CHIP_TRANQUILIZER: i64 = 94;
const CHIP_ANTIDOTE: i64 = 110;
const CHIP_RESURRECTION: i64 = 84;

#[test]
fn poison_via_venom() {
    let f = arena(Entity::new(1, "Bot", 0, 0), Entity::new(2, "Foe", 33, 1));
    run(&f, &format!("useChip({CHIP_VENOM}, 2)")); // 15–20/turn for 3 turns
    f.borrow_mut().tick_effects(2); // one turn of poison
    let life = f.borrow().life(2).unwrap();
    assert!((80..=85).contains(&life), "after 1 poison tick foe at {life}, expected 80..=85");
}

#[test]
fn vulnerability_via_fracture() {
    let f = arena(Entity::new(1, "Bot", 0, 0), Entity::new(2, "Foe", 33, 1));
    run(&f, &format!("useChip({CHIP_FRACTURE}, 2)"));
    // Relative vulnerability is stored as a negative relative shield (20–25).
    let rel = f.borrow().relative_shield(2);
    assert!((-25..=-20).contains(&rel), "fracture gave relative shield {rel}, expected -25..=-20");
}

#[test]
fn regeneration_over_time() {
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_life(150),
        Entity::new(2, "Foe", 33, 1),
    );
    f.borrow_mut().deal_damage(1, 100); // Bot → 50
    run(&f, &format!("useChip({CHIP_REGENERATION}, getEntity())")); // +50/turn
    f.borrow_mut().tick_effects(1);
    assert_eq!(f.borrow().life(1), Some(100));
}

#[test]
fn shackle_via_tranquilizer() {
    let f = arena(Entity::new(1, "Bot", 0, 0), Entity::new(2, "Foe", 33, 1));
    run(&f, &format!("useChip({CHIP_TRANQUILIZER}, 2)"));
    // Strength shackle is a negative strength buff (magic 0 → 20–25).
    let s = f.borrow().strength(2).unwrap();
    assert!((-25..=-20).contains(&s), "tranquilizer gave strength {s}, expected -25..=-20");
}

#[test]
fn antidote_clears_poison() {
    let mut bot = Entity::new(1, "Bot", 0, 0);
    bot.effects.push(ActiveEffect { kind: EffectKind::Poison, value: 10, turns: 5 });
    let f = arena(bot, Entity::new(2, "Foe", 33, 1));
    run(&f, &format!("useChip({CHIP_ANTIDOTE}, getEntity())")); // clears poison
    f.borrow_mut().tick_effects(1);
    assert_eq!(f.borrow().life(1), Some(100), "antidote should have removed the poison");
}

#[test]
fn resurrect_a_dead_entity() {
    // Foe at cell 11 (distance 2 from Bot) — within resurrection's 1–2 range.
    let f = shared(
        Fight::new(10, 10, 1)
            .with_entity(Entity::new(1, "Bot", 0, 0).with_points(5, 20)) // resurrection costs 15 TP
            .with_entity(Entity::new(2, "Foe", 11, 1)),
    );
    f.borrow_mut().deal_damage(2, 999); // kill the foe
    assert_eq!(f.borrow().life(2), Some(0));
    run(&f, &format!("useChip({CHIP_RESURRECTION}, 2)"));
    assert_eq!(f.borrow().life(2), Some(100), "resurrection should revive the foe");
}

#[test]
fn vitality_raises_max_and_heals() {
    let f = arena(Entity::new(1, "Bot", 0, 0).with_life(100), Entity::new(2, "Foe", 33, 1));
    f.borrow_mut().grant_vitality(1, 50);
    assert_eq!(f.borrow().max_life(1), Some(150));
    assert_eq!(f.borrow().life(1), Some(150));
}

const WEAPON_GRENADE_LAUNCHER: i64 = 43; // area 4, range 4–7

#[test]
fn line_of_sight_blocks_attacks() {
    // Obstacle at (2,2)=22 sits on the (0,0)→(3,3) line.
    let f = shared(
        Fight::new(10, 10, 1)
            .with_entity(Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_PISTOL))
            .with_entity(Entity::new(2, "Foe", 33, 1))
            .with_obstacle(22),
    );
    assert_eq!(run(&f, "return lineOfSight(0, 33)"), "false");
    assert_eq!(run(&f, "return lineOfSight(0, 3)"), "true"); // clear horizontal line
    assert_eq!(run(&f, "return useWeapon(2)"), "-3"); // USE_INVALID_POSITION
    assert_eq!(run(&f, "return getCellContent(22)"), "1"); // CELL_OBSTACLE
}

#[test]
fn area_of_effect_hits_neighbours() {
    // Grenade (area 4) centered on Foe-A at 33 also hits Foe-B at 34.
    let f = shared(
        Fight::new(10, 10, 1)
            .with_entity(Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_GRENADE_LAUNCHER))
            .with_entity(Entity::new(2, "FoeA", 33, 1))
            .with_entity(Entity::new(3, "FoeB", 34, 1)),
    );
    run(&f, "useWeapon(2)");
    assert!(f.borrow().life(2).unwrap() < 100, "primary target should be hit");
    assert!(f.borrow().life(3).unwrap() < 100, "neighbour should be caught in the blast");
}

#[test]
fn cooldowns_block_reuse() {
    // cure has a 2-turn cooldown; the second use the same turn is blocked.
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_points(5, 20),
        Entity::new(2, "Foe", 33, 1),
    );
    assert_eq!(run(&f, &format!("return useChip({CHIP_CURE}, getEntity())")), "1"); // USE_SUCCESS
    assert_eq!(run(&f, &format!("return getCooldown({CHIP_CURE})")), "2");
    assert_eq!(run(&f, &format!("return useChip({CHIP_CURE}, getEntity())")), "-4"); // USE_TOO_MANY_USES
    // After a turn (regen ticks cooldowns), it drops to 1.
    f.borrow_mut().regen(1);
    assert_eq!(run(&f, &format!("return getCooldown({CHIP_CURE})")), "1");
}

#[test]
fn pathfinding_around_obstacles() {
    // Clear 5×5: corner-to-corner is the Manhattan distance.
    let f = shared(Fight::new(5, 5, 1).with_entity(Entity::new(1, "Bot", 0, 0)));
    assert_eq!(run(&f, "return getPathLength(0, 2)"), "2");
    // Surround cell 12=(2,2) with obstacles → unreachable.
    let f = shared(
        Fight::new(5, 5, 1)
            .with_entity(Entity::new(1, "Bot", 0, 0))
            .with_obstacle(7) // (2,1)
            .with_obstacle(17) // (2,3)
            .with_obstacle(11) // (1,2)
            .with_obstacle(13), // (3,2)
    );
    assert_eq!(run(&f, "return getPathLength(0, 12)"), "-1");
    assert_eq!(run(&f, "return getPath(0, 12)"), "null");
}

#[test]
fn team_and_turn_queries() {
    let f = shared(
        Fight::new(10, 10, 1)
            .with_entity(Entity::new(1, "Bot", 0, 0))
            .with_entity(Entity::new(2, "Ally", 5, 0))
            .with_entity(Entity::new(3, "Foe", 33, 1)),
    );
    assert_eq!(run(&f, "return getEnemies()"), "[3]");
    assert_eq!(run(&f, "return getAllies()"), "[2]");
    assert_eq!(run(&f, "return getEnemiesCount()"), "1");
    assert_eq!(run(&f, "return getNearestEnemy()"), "3");

    // getTurn reflects the turn loop: a passive AI logs the turn each round.
    let mut ais: HashMap<i64, HirFile> = HashMap::new();
    ais.insert(1, compile("say(\"\" + getTurn())"));
    run_fight(&f, &ais, 3, 4, false).expect("runs");
    assert_eq!(f.borrow().log().first().map(|(_, m)| m.clone()), Some("1".to_string()));
}

#[test]
fn turn_loop_to_victory() {
    // Bot: strength 100 + pistol (12 TP → 4 hits of 30–40) kills the 100-life
    // foe; Foe has no AI.
    let f = arena(
        Entity::new(1, "Bot", 0, 0).with_weapon(WEAPON_PISTOL).with_strength(100).with_points(5, 12),
        Entity::new(2, "Foe", 33, 1),
    );
    let mut ais: HashMap<i64, HirFile> = HashMap::new();
    ais.insert(1, compile("while (getTP() >= 3) { useWeapon(2) }"));
    let outcome = run_fight(&f, &ais, 10, 4, false).expect("fight runs");
    assert_eq!(outcome.winner_team, Some(0));
    assert!(f.borrow().life(2).unwrap() <= 0, "foe should be dead");
}
