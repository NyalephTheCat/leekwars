//! Official builtin wrappers — ports of the `com.leekwars.generator.classes`
//! dispatch layer (`FightClass`, `EntityClass`, `WeaponClass`) over the
//! official [`State`].
//!
//! Where [`crate::builtins::call_game_builtin`] implements the fight
//! functions with engine-native semantics against a
//! [`GameHost`](crate::GameHost), this module reproduces the *reference*
//! semantics (argument validation, warning-then-`false` paths, exact return
//! codes) for the conformance runner. It grows function-by-function as the
//! oracle corpus exercises them; an unknown name returns [`Value::Null`].

use leek_runtime::Value;

use crate::attack::{EffectType, EntityState};
use crate::state::{
    ChipSpec, ERROR_HELP_PAGE_LINK, FARMER_LOG_ACTION_DENIED_IN_HOOK, FARMER_LOG_BULB_WITHOUT_AI,
    FARMER_LOG_LOADOUT_FORGOTTEN_ALREADY_EQUIPPED, FARMER_LOG_LOADOUT_NOT_FOUND,
    FARMER_LOG_SET_LOADOUT_NO_RESTAT_POTION, FARMER_LOG_SET_LOADOUT_OUT_OF_HOOK, LOG_SSTANDARD,
    LOG_SWARNING, State, USE_RESURRECT_INVALID_ENTITY,
};

/// Dispatch one official fight function for the entity `current` (the fid
/// the running AI controls — `ai.getEntity()`).
#[must_use]
// Cell and coordinate arguments arrive as LeekScript `i64` and are narrowed
// with `as i32`, which is Java's `(int)` cast: the low 32 bits, sign-extended.
// That is deliberate and observable — `getCellX(-9223372036854775808)` narrows
// to 0 and answers about cell 0, it does not answer `null` — so the narrowing
// must stay a truncation and not become a `try_from`. Every such site below is
// followed by a range check (`Map::get_cell`, `Map::get_cell_xy`,
// `resolve_entity`) that decides what an out-of-board value means, and those
// are what turn a genuinely off-board id into the game's sentinel.
//
// `cast_possible_wrap` covers the other direction: `usize` fids and cell
// indices going back out as `i64`/`i32`. Those are bounded by construction —
// a fid is `< fighters.len()` (at most a few dozen) and a cell index is
// `< 613` — so no value that exists can wrap.
#[allow(clippy::cast_possible_wrap, clippy::cast_possible_truncation)]
pub fn call_official_builtin(
    state: &mut State,
    current: usize,
    name: &str,
    args: &[Value],
) -> Value {
    // `Error.ENTITY_DIED` — when a cast kills the *caster* (damage return,
    // poison return, ...), Java's `useWeapon`/`useChip` throw ENTITY_DIED and
    // `EntityAI.runIA` catches it silently ("OK, c'est normal"): the turn
    // just ends, no AI-error log. The Rust runtimes have no abort channel
    // through the AI, so the observably-identical port is to no-op every
    // game call once the caster is dead — nothing a dead entity calls logs
    // an action in Java either. During a `beforeFight`/`afterFight` hook no
    // turn is active, so this no-op doesn't apply: the hook runs (with
    // `mEntity = mInitialEntity`) and per-function gating handles the rest.
    if !state.is_in_hook() && state.fighters[current].is_dead() {
        return Value::Null;
    }

    let int_arg = |i: usize| args.get(i).map_or(0, Value::to_long);

    match name {
        // ---- FightClass ----
        "getTurn" => Value::Int(i64::from(state.order.turn())),
        "getNearestEnemy" => Value::Int(nearest_enemy(state, current)),
        "moveToward" => {
            // moveToward(leek_id[, pm_to_use]) — pm defaults to -1 (all MP).
            if deny_during_hook(state, current, "moveToward") {
                return Value::Int(0);
            }
            let pm = args.get(1).map_or(-1, Value::to_long);
            Value::Int(state.move_toward(current, int_arg(0), pm))
        }
        "moveTowardCell" => {
            // moveTowardCell(cell_id[, pm_to_use]) — pm defaults to the
            // entity's MP (the Java overload passes getMP() explicitly;
            // the state clamps to MP either way).
            if deny_during_hook(state, current, "moveTowardCell") {
                return Value::Int(0);
            }
            let pm = args.get(1).map_or(-1, Value::to_long);
            Value::Int(state.move_toward_cell(current, int_arg(0), pm))
        }
        "getWinner" => Value::Int(i64::from(state.win_team)),
        "setLoadout" => Value::Bool(set_loadout(state, current, args)),

        // ---- FieldClass ----
        // The AI-visible x axis is shifted: `getCellFromXY(x, y)` looks up
        // `(x + width - 1, y)` and `getCellX` shifts back.
        "getCellFromXY" => {
            // `wrapping_add`, not `+`: Java computes `(int) x + width - 1` in
            // `int`, which wraps. Rust's `+` panics on overflow in any build
            // with overflow checks on — every debug and test build — so
            // `getCellFromXY(2147483647, 0)` used to abort the fight there
            // while quietly wrapping in release. Wrapping is both the
            // faithful port and the same answer either way: a sum that far
            // out misses the coordinate table and yields `null`.
            let x = (int_arg(0) as i32).wrapping_add(state.map.width - 1);
            state
                .map
                .get_cell_xy(x, int_arg(1) as i32)
                .map_or(Value::Null, |c| Value::Int(c as i64))
        }
        // These three narrow and then hand the result straight to
        // `Map::get_cell`, whose `id < 0 || id as usize >= nb_cells` check
        // (map.rs) is what decides the answer. No arithmetic happens on the
        // narrowed value first, so there is nothing left to overflow.
        "getCellX" => state
            .map
            .get_cell(int_arg(0) as i32)
            .map_or(Value::Null, |c| {
                Value::Int(i64::from(state.map.cells[c].x - state.map.width + 1))
            }),
        "getCellY" => state
            .map
            .get_cell(int_arg(0) as i32)
            .map_or(Value::Null, |c| Value::Int(i64::from(state.map.cells[c].y))),
        // A missing cell counts as an obstacle.
        "isObstacle" => Value::Bool(
            state
                .map
                .get_cell(int_arg(0) as i32)
                .is_none_or(|c| !state.map.cells[c].walkable),
        ),

        // ---- EntityClass ----
        "getCell" => get_cell(state, current, args.first()),
        "setWeapon" => {
            if deny_during_hook(state, current, "setWeapon") {
                return Value::Bool(false);
            }
            Value::Bool(set_weapon(state, current, int_arg(0)))
        }
        // isStatic([entity]) — no arg (or null) means self; a non-entity
        // argument is false (Java's `instanceof Number` + lookup-miss paths).
        "isStatic" => Value::Bool(is_static(state, current, args.first())),

        // ---- WeaponClass ----
        "useWeapon" => {
            if deny_during_hook(state, current, "useWeapon") {
                return Value::Int(-1);
            }
            Value::Int(use_weapon(state, current, int_arg(0)))
        }

        // ---- ChipClass ----
        "useChip" => {
            // useChip(chip_id[, leek_id]) — the target defaults to self.
            if deny_during_hook(state, current, "useChip") {
                return Value::Int(-1);
            }
            #[allow(clippy::cast_possible_wrap)]
            let target = args.get(1).map_or(current as i64, Value::to_long);
            Value::Int(use_chip(state, current, int_arg(0), target))
        }
        "useChipOnCell" => {
            // useChipOnCell(chip_id, cell_id) — equipped chip + valid cell,
            // straight to `State.useChip` at that cell.
            if deny_during_hook(state, current, "useChipOnCell") {
                return Value::Int(-1);
            }
            Value::Int(use_chip_on_cell(state, current, int_arg(0), int_arg(1)))
        }
        "getCellToUseChip" => {
            // getCellToUseChip(chip_id, leek_id) — nearest cell the chip
            // could be cast from to hit the target (template registry, NOT
            // the equipped list). The 3-arg ignore-list form is unported
            // (corpus-first).
            Value::Int(get_cell_to_use_chip(state, current, int_arg(0), int_arg(1)))
        }
        "summon" => {
            // summon(chip_id, cell_id, ai_function[, name])
            Value::Int(summon(state, current, args))
        }
        "resurrect" => {
            // resurrect(entity, cell) — `ChipClass.resurrect`.
            Value::Int(resurrect(state, current, args))
        }

        // ---- EntityClass (summons) ----
        // `EntityClass.getEntity(ai)` — the current entity's fid.
        #[allow(clippy::cast_possible_wrap)]
        "getEntity" => Value::Int(current as i64),
        "getSummons" => get_summons(state, current, args.first()),
        "isSummon" => match resolve_entity(state, current, args.first()) {
            Some(fid) => Value::Bool(state.fighters[fid].is_summon()),
            None => Value::Null,
        },
        "getSummoner" => match resolve_entity(state, current, args.first()) {
            // -1 for non-summons (`getSummoner()` has no null path there).
            Some(fid) => Value::Int(
                state.fighters[fid]
                    .summoner
                    .map_or(-1, |owner| owner as i64),
            ),
            None => Value::Null,
        },
        "getBirthTurn" => match resolve_entity(state, current, args.first()) {
            Some(fid) => Value::Int(i64::from(state.fighters[fid].birth_turn)),
            None => Value::Null,
        },

        _ => Value::Null,
    }
}

/// The combat/movement gate shared by `WeaponClass`/`ChipClass`/`FightClass`/
/// `EntityClass`: during a `beforeFight()`/`afterFight()` hook no turn is
/// active, so an action that would consume TP/MP or trigger effects is
/// refused with an `ACTION_DENIED_IN_HOOK` warning. Returns `true` when the
/// caller should bail with its "denied" value.
fn deny_during_hook(state: &mut State, current: usize, func_name: &str) -> bool {
    if state.is_in_hook() {
        state.add_system_log(
            current,
            LOG_SWARNING,
            FARMER_LOG_ACTION_DENIED_IN_HOOK,
            Some(&[func_name]),
        );
        true
    } else {
        false
    }
}

/// `FightClass.setLoadout(name[, changeStats])` — only valid inside the
/// `beforeFight()` hook. Looks up the named loadout on the running entity and
/// applies it (weapons/chips, and stats when `changeStats` and they differ,
/// spending a restat potion). Forgotten weapons already worn by teammates of
/// the same farmer are reserved so they aren't duplicated. Mirrors the exact
/// check order and warning paths of `FightClass.setLoadout`.
fn set_loadout(state: &mut State, current: usize, args: &[Value]) -> bool {
    if !state.is_in_before_fight_hook() {
        state.add_system_log(
            current,
            LOG_SWARNING,
            FARMER_LOG_SET_LOADOUT_OUT_OF_HOOK,
            Some(&[]),
        );
        return false;
    }
    // `ai.string(nameObject)` — the bare LeekScript string of the argument.
    let name = match args.first() {
        Some(Value::String(s)) => s.to_string(),
        Some(v) => v.to_string(),
        None => String::new(),
    };
    // `ai.bool(changeStats)` — defaults to the 1-arg overload's `true`.
    let change_stats = args.get(1).is_none_or(Value::is_truthy);

    let Some(loadout) = state.get_loadout(current, &name) else {
        state.add_system_log(
            current,
            LOG_SWARNING,
            FARMER_LOG_LOADOUT_NOT_FOUND,
            Some(&[&name]),
        );
        return false;
    };

    // Forgotten weapons are unique per farmer: collect those already equipped
    // on other entities of the same farmer so we don't duplicate them here.
    let farmer = state.fighters[current].farmer;
    let mut reserved_forgotten: std::collections::HashSet<i32> = std::collections::HashSet::new();
    if farmer > 0 {
        for fid in 0..state.fighters.len() {
            if fid == current || state.fighters[fid].farmer != farmer {
                continue;
            }
            let worn: Vec<i32> = state.fighters[fid].weapons.clone();
            for w in worn {
                if state.weapon_specs.get(&w).is_some_and(|s| s.forgotten) {
                    reserved_forgotten.insert(w);
                }
            }
        }
    }

    // Pre-check whether the stats actually differ so we don't waste a potion
    // on an identical loadout.
    let mut apply_stats = change_stats && state.loadout_stats_differ(current, &loadout);
    if apply_stats && !state.consume_restat_potion(farmer) {
        apply_stats = false;
        state.add_system_log(
            current,
            LOG_SWARNING,
            FARMER_LOG_SET_LOADOUT_NO_RESTAT_POTION,
            Some(&[&name]),
        );
    }

    let result = state.apply_loadout(current, &loadout, &reserved_forgotten, apply_stats);
    if result.no_forgotten_available {
        state.add_system_log(
            current,
            LOG_SWARNING,
            FARMER_LOG_LOADOUT_FORGOTTEN_ALREADY_EQUIPPED,
            Some(&[&name]),
        );
    }
    true
}

/// `FightClass.getNearestEnemy` — nearest by **squared Euclidean** distance
/// (`Map.getDistance2`), first-seen wins ties; `-1` when none.
#[allow(clippy::cast_possible_wrap)]
fn nearest_enemy(state: &State, current: usize) -> i64 {
    let Some(my_cell) = state.fighters[current].cell else {
        return -1;
    };
    let my_team = state.fighters[current].team;
    let mut dist = -1;
    let mut nearest = -1;
    for (t, team) in state.teams.iter().enumerate() {
        if t == my_team {
            continue;
        }
        for &fid in &team.fighters {
            let f = &state.fighters[fid];
            if f.is_dead() {
                continue;
            }
            let Some(cell) = f.cell else { continue };
            let d = state.map.get_distance_sq(my_cell, cell);
            if d < dist || dist == -1 {
                dist = d;
                nearest = fid as i64;
            }
        }
    }
    nearest
}

/// `EntityClass.setWeapon` — template must exist and be owned; then
/// `State.setWeapon` (1 TP, logs even on re-equip).
fn set_weapon(state: &mut State, current: usize, weapon_id: i64) -> bool {
    let Ok(weapon_id) = i32::try_from(weapon_id) else {
        return false;
    };
    if !state.weapon_specs.contains_key(&weapon_id) {
        return false; // WEAPON_NOT_EXISTS warning
    }
    if !state.fighters[current].has_weapon(weapon_id) {
        return false; // WEAPON_NOT_EQUIPPED warning
    }
    state.set_weapon(current, weapon_id)
}

/// `EntityClass.getCell()` / `getCell(entity)` — the entity's cell id, or
/// null when it has none (dead) or the argument doesn't resolve.
#[allow(clippy::cast_possible_wrap)]
fn get_cell(state: &State, current: usize, arg: Option<&Value>) -> Value {
    let fid = match arg {
        None | Some(Value::Null) => Some(current),
        Some(v) => usize::try_from(v.to_long())
            .ok()
            .filter(|&t| t < state.fighters.len()),
    };
    match fid.and_then(|f| state.fighters[f].cell) {
        Some(cell) => Value::Int(cell as i64),
        None => Value::Null,
    }
}

/// `EntityClass.isStatic()` / `isStatic(entity)` — whether the entity has
/// the STATIC state. No arg or null means self; an unresolvable entity is
/// `false`.
fn is_static(state: &State, current: usize, arg: Option<&Value>) -> bool {
    let fid = match arg {
        None | Some(Value::Null) => Some(current),
        Some(v) => usize::try_from(v.to_long())
            .ok()
            .filter(|&t| t < state.fighters.len()),
    };
    fid.is_some_and(|f| state.fighters[f].has_state(EntityState::Static))
}

/// The entity argument convention shared by the `EntityClass` getters: no
/// arg (or null) means self; a number resolves through `Fight.getEntity`
/// (dead entities and summons stay resolvable — they are only force-removed
/// after the fight); anything else is `None` (the Java overloads return
/// null).
fn resolve_entity(state: &State, current: usize, arg: Option<&Value>) -> Option<usize> {
    match arg {
        None | Some(Value::Null) => Some(current),
        Some(v) => usize::try_from(v.to_long())
            .ok()
            .filter(|&t| t < state.fighters.len()),
    }
}

/// Whether a chip carries a `TYPE_SUMMON` effect line — the `Fight.useChip`
/// intercept test.
fn has_summon_effect(spec: &ChipSpec) -> bool {
    spec.effects.iter().any(|p| p.effect == EffectType::Summon)
}

/// `ChipClass.summon(chip_id, cell_id, ai_function[, name])` — exact check
/// order: the cell resolves first, then the function value, then the
/// equipped chip; then `Fight.summonEntity` (the state ladder plus the
/// `BulbAI` attachment on success).
fn summon(state: &mut State, current: usize, args: &[Value]) -> i64 {
    let Ok(cell_id) = i32::try_from(args.get(1).map_or(0, Value::to_long)) else {
        return -1;
    };
    let Some(target_cell) = state.map.get_cell(cell_id) else {
        return -1;
    };
    // `!(summonAI instanceof FunctionLeekValue)` — null included.
    let Some(ai_fn @ Value::Function(_)) = args.get(2) else {
        return -1;
    };
    let ai_fn = ai_fn.clone();
    let Ok(chip) = i32::try_from(args.first().map_or(0, Value::to_long)) else {
        return -1;
    };
    if !state.fighters[current].chips.contains(&chip) {
        return -1; // CHIP_NOT_EXISTS / CHIP_NOT_EQUIPPED warning
    }
    let name = match args.get(3) {
        Some(Value::String(s)) => Some(s.clone()),
        _ => None,
    };
    let (result, bulb) = state.summon_entity(
        current,
        target_cell,
        chip,
        name.as_ref().map(|s| s.as_str()),
    );
    // `Fight.summonEntity` — attach the AI function to the new bulb.
    if result > 0
        && let Some(bulb) = bulb
    {
        state.summon_ais.insert(bulb, ai_fn);
    }
    i64::from(result)
}

/// `ChipClass.resurrect(entity, cell)` — exact check order: the cell
/// resolves first (-1), then the entity via `Fight.getEntity` (dead fids
/// resolvable) which must exist and BE dead (-6), then the equipped chip:
/// template 84 (`CHIP_RESURRECTION`), falling back to 415 ("Awakening",
/// the full-life variant); neither equipped → -1 (Java adds a
/// CHIP_NOT_EXISTS / CHIP_NOT_EQUIPPED warning there — corpus AIs always
/// equip 84). Then `State.resurrectEntity`.
fn resurrect(state: &mut State, current: usize, args: &[Value]) -> i64 {
    let Ok(cell_id) = i32::try_from(args.get(1).map_or(0, Value::to_long)) else {
        return -1;
    };
    let Some(target_cell) = state.map.get_cell(cell_id) else {
        return -1;
    };
    let target = usize::try_from(args.first().map_or(-1, Value::to_long))
        .ok()
        .filter(|&t| t < state.fighters.len());
    let Some(target) = target else {
        return i64::from(USE_RESURRECT_INVALID_ENTITY);
    };
    if !state.fighters[target].is_dead() {
        return i64::from(USE_RESURRECT_INVALID_ENTITY);
    }
    let template = [84, 415]
        .into_iter()
        .find(|id| state.fighters[current].chips.contains(id));
    let Some(template) = template else {
        return -1;
    };
    let full_life = state.fighters[current].chips.contains(&415);
    i64::from(state.resurrect_entity(current, target_cell, template, target, full_life))
}

/// `EntityClass.getSummons([entity])` — the fids of the entity's *alive*
/// summons (`Entity.getSummons(false)`), in team-list order.
#[allow(clippy::cast_possible_wrap)]
fn get_summons(state: &State, current: usize, arg: Option<&Value>) -> Value {
    let Some(fid) = resolve_entity(state, current, arg) else {
        return Value::Null;
    };
    let team = &state.teams[state.fighters[fid].team];
    let summons: Vec<Value> = team
        .fighters
        .iter()
        .filter(|&&f| !state.fighters[f].is_dead() && state.fighters[f].summoner == Some(fid))
        .map(|&f| Value::Int(f as i64))
        .collect();
    Value::Array(std::rc::Rc::new(std::cell::RefCell::new(summons)))
}

/// `ChipClass.useChipOnCell(chip_id, cell_id)` — the chip must be *equipped*
/// and the cell must exist; then `Fight.useChip` at that cell. `-1` when
/// either check fails (warning paths).
fn use_chip_on_cell(state: &mut State, current: usize, chip_id: i64, cell_id: i64) -> i64 {
    let Ok(chip) = i32::try_from(chip_id) else {
        return -1;
    };
    if !state.fighters[current].chips.contains(&chip) {
        return -1; // CHIP_NOT_EXISTS / CHIP_NOT_EQUIPPED warning
    }
    let Ok(cell_id) = i32::try_from(cell_id) else {
        return -1;
    };
    let Some(target_cell) = state.map.get_cell(cell_id) else {
        return -1;
    };
    // `Fight.useChip` — a summon chip goes down the BULB_WITHOUT_AI path:
    // two system logs, then the bulb is created with no AI function and
    // idles.
    if state.chip_specs.get(&chip).is_some_and(has_summon_effect) {
        state.add_system_log(current, LOG_SWARNING, FARMER_LOG_BULB_WITHOUT_AI, Some(&[]));
        state.add_system_log(
            current,
            LOG_SSTANDARD,
            ERROR_HELP_PAGE_LINK,
            Some(&["summons"]),
        );
        return i64::from(state.summon_entity(current, target_cell, chip, None).0);
    }
    i64::from(state.use_chip(current, target_cell, chip))
}

/// `FightClass.getCellToUseChip(chip_id, leek_id)` — resolve the chip from
/// the *template registry* (`Chips.getChip` — being equipped is NOT
/// required), collect every castable cell for the target via
/// `getPossibleCastCellsForTarget` (the caster's own cell counts as
/// available), and return the caster's cell if it qualifies, else the end of
/// the multi-goal A* path toward the nearest one. `-1` when nothing
/// qualifies.
#[allow(clippy::cast_possible_wrap)]
fn get_cell_to_use_chip(state: &mut State, current: usize, chip_id: i64, leek_id: i64) -> i64 {
    let Ok(chip) = i32::try_from(chip_id) else {
        return -1;
    };
    let Some(spec) = state.chip_specs.get(&chip) else {
        return -1;
    };
    let (min_range, max_range, launch_type, needs_los) = (
        spec.min_range,
        spec.max_range,
        spec.launch_type,
        spec.needs_los,
    );
    let target = usize::try_from(leek_id)
        .ok()
        .filter(|&t| t < state.fighters.len());
    let Some(target) = target else { return -1 };
    // A dead target has no cell — Java's getPossibleCastCellsForTarget
    // returns null for a null target cell.
    let Some(target_cell) = state.fighters[target].cell else {
        return -1;
    };
    let Some(my_cell) = state.fighters[current].cell else {
        return -1;
    };
    let ignore = [my_cell];
    let possible = state.map.get_possible_cast_cells_for_target(
        min_range,
        max_range,
        launch_type,
        needs_los,
        target_cell,
        &ignore,
    );
    if possible.is_empty() {
        return -1;
    }
    if possible.contains(&my_cell) {
        return my_cell as i64;
    }
    match state.map.get_astar_path(my_cell, &possible, &ignore) {
        // An empty path means "already there" in Java — return the own cell.
        Some(path) => path.last().map_or(my_cell as i64, |&c| c as i64),
        None => -1,
    }
}

/// `ChipClass.useChip(chip_id, leek_id)` — the chip must be *equipped*
/// (`entity.getChip`), the target must exist and be alive (the caster itself
/// is a valid target); then `State.useChip` at the target's cell. `-1` when
/// either check fails (warning paths).
fn use_chip(state: &mut State, current: usize, chip_id: i64, leek_id: i64) -> i64 {
    let Ok(chip) = i32::try_from(chip_id) else {
        return -1;
    };
    if !state.fighters[current].chips.contains(&chip) {
        return -1; // CHIP_NOT_EXISTS / CHIP_NOT_EQUIPPED warning
    }
    let target = usize::try_from(leek_id)
        .ok()
        .filter(|&t| t < state.fighters.len() && !state.fighters[t].is_dead());
    let Some(target) = target else { return -1 };
    let Some(target_cell) = state.fighters[target].cell else {
        return -1;
    };
    // `Fight.useChip` — a summon chip goes down the BULB_WITHOUT_AI path:
    // two system logs, then the bulb is created with no AI function and
    // idles.
    if state.chip_specs.get(&chip).is_some_and(has_summon_effect) {
        state.add_system_log(current, LOG_SWARNING, FARMER_LOG_BULB_WITHOUT_AI, Some(&[]));
        state.add_system_log(
            current,
            LOG_SSTANDARD,
            ERROR_HELP_PAGE_LINK,
            Some(&["summons"]),
        );
        return i64::from(state.summon_entity(current, target_cell, chip, None).0);
    }
    i64::from(state.use_chip(current, target_cell, chip))
}

/// `WeaponClass.useWeapon(leek_id)` — resolve the target entity, fire at its
/// cell. `-1` when the target is invalid (missing, self, or dead).
fn use_weapon(state: &mut State, current: usize, leek_id: i64) -> i64 {
    let target = usize::try_from(leek_id)
        .ok()
        .filter(|&t| t < state.fighters.len() && t != current && !state.fighters[t].is_dead());
    let Some(target) = target else { return -1 };
    let Some(target_cell) = state.fighters[target].cell else {
        return -1;
    };
    i64::from(state.use_weapon(current, target_cell))
}

#[cfg(test)]
mod tests {
    use leek_runtime::Value;

    use super::call_official_builtin;
    use crate::state::{Fighter, STAT_LIFE, STAT_MP, STAT_TP, State, Stats};

    /// Every extreme an AI can hand a builtin. LeekScript integers are `i64`,
    /// and `Value::to_long` produces any of these from an `Int`, from a
    /// `BigInt`'s low 64 bits, or from a parsed `String` — so every one of
    /// them is reachable from ordinary AI code.
    const EXTREMES: &[i64] = &[
        i64::MIN,
        -2_147_483_649, // i32::MIN - 1
        -2_147_483_648, // i32::MIN
        -613,
        -1,
        0,
        1,
        612, // the last cell
        613, // one past the board
        2_147_483_630,
        2_147_483_647, // i32::MAX — `getCellFromXY` used to overflow here
        2_147_483_648, // i32::MAX + 1
        4_294_967_296, // truncates to 0 in i32
        i64::MAX,
    ];

    /// One living leek on a mid-board cell, enough for the field and
    /// movement builtins to have something to answer about.
    fn one_leek() -> State {
        let mut stats = Stats::default();
        stats.set(STAT_LIFE, 100);
        stats.set(STAT_TP, 10);
        stats.set(STAT_MP, 5);
        let mut state = State::new(42);
        let fid = state.add_entity(0, Fighter::new(0, 1, "test".into(), 0, stats));
        state.place_entity(fid, 306);
        state
    }

    /// `Value` has no `PartialEq` (an `Array` compares by identity, not by
    /// contents); `identity_eq` is the comparison these scalars want.
    #[track_caller]
    fn assert_value(got: &Value, want: &Value, what: &str) {
        assert!(
            got.identity_eq(want),
            "{what}: expected {want:?}, got {got:?}"
        );
    }

    /// No argument an AI can write may panic a builtin.
    ///
    /// This is the regression test for the `getCellFromXY` overflow:
    /// `getCellFromXY(2147483647, 0)` computed `(int) x + width - 1` in
    /// `i32` and overflowed, which aborts the whole fight in any build with
    /// overflow checks on — every debug and test build, since the workspace
    /// sets no `[profile]` overrides. In release it wrapped instead, so the
    /// two builds disagreed about the same fight.
    #[test]
    fn extreme_arguments_never_panic_a_builtin() {
        const NAMES: &[&str] = &[
            "getCellFromXY",
            "getCellX",
            "getCellY",
            "isObstacle",
            "getCell",
            "moveToward",
            "moveTowardCell",
            "useChip",
            "useChipOnCell",
            "useWeapon",
            "setWeapon",
            "summon",
            "getCellToUseChip",
            "isStatic",
        ];
        for name in NAMES {
            for &a in EXTREMES {
                for &b in EXTREMES {
                    let mut state = one_leek();
                    // The assertion is "this returns at all".
                    let _ =
                        call_official_builtin(&mut state, 0, name, &[Value::Int(a), Value::Int(b)]);
                }
            }
        }
    }

    /// The exact call that aborted the fight, kept on its own so a
    /// regression names itself.
    #[test]
    fn get_cell_from_xy_survives_an_i32_max_coordinate() {
        let mut state = one_leek();
        for x in [2_147_483_647_i64, 2_147_483_630] {
            let got = call_official_builtin(
                &mut state,
                0,
                "getCellFromXY",
                &[Value::Int(x), Value::Int(0)],
            );
            // The sum wraps to somewhere near `i32::MIN`, which cannot be in
            // the coordinate table, so the lookup misses and the AI sees
            // `null` — what the reference answers for an off-board
            // coordinate, and what release builds already did.
            assert_value(&got, &Value::Null, &format!("getCellFromXY({x}, 0)"));
        }
        // `i64::MAX` narrows to -1, and -1 + 17 = 16 *is* on the board, so
        // this one aliases rather than missing — the same `(int)` narrowing
        // documented on `call_official_builtin`.
        let aliased = call_official_builtin(
            &mut state,
            0,
            "getCellFromXY",
            &[Value::Int(i64::MAX), Value::Int(0)],
        );
        let direct = call_official_builtin(
            &mut state,
            0,
            "getCellFromXY",
            &[Value::Int(-1), Value::Int(0)],
        );
        assert_value(&aliased, &direct, "i64::MAX must answer exactly as -1 does");
    }

    /// In-range coordinates still round-trip through the shifted x axis.
    #[test]
    fn get_cell_from_xy_round_trips_in_range() {
        let mut state = one_leek();
        let cell = call_official_builtin(
            &mut state,
            0,
            "getCellFromXY",
            &[Value::Int(0), Value::Int(0)],
        );
        let Value::Int(id) = cell else {
            panic!("(0, 0) is on the board, got {cell:?}");
        };
        let x = call_official_builtin(&mut state, 0, "getCellX", &[Value::Int(id)]);
        let y = call_official_builtin(&mut state, 0, "getCellY", &[Value::Int(id)]);
        assert_value(&x, &Value::Int(0), "getCellX of the (0,0) cell");
        assert_value(&y, &Value::Int(0), "getCellY of the (0,0) cell");
    }

    /// An off-board cell id that still fits in an `int` answers the
    /// sentinel: `Map::get_cell` range-checks, and these three only narrow
    /// in front of it.
    #[test]
    fn off_board_cell_ids_answer_the_sentinel() {
        let mut state = one_leek();
        for id in [-1_i64, 613, 10_000, 2_147_483_647, -2_147_483_648] {
            for name in ["getCellX", "getCellY"] {
                let got = call_official_builtin(&mut state, 0, name, &[Value::Int(id)]);
                assert_value(&got, &Value::Null, &format!("{name}({id})"));
            }
            // A cell that does not exist counts as an obstacle.
            let got = call_official_builtin(&mut state, 0, "isObstacle", &[Value::Int(id)]);
            assert_value(&got, &Value::Bool(true), &format!("isObstacle({id})"));
        }
    }

    /// A cell id outside `int` aliases onto a real cell, on purpose.
    ///
    /// The narrowing is Java's `(int)` cast — low 32 bits, sign-extended —
    /// and it happens *before* the range check, so `getCellX(i64::MIN)`
    /// narrows to 0 and answers about cell 0 rather than answering `null`.
    /// That is the reference engine's behaviour, not an oversight, and this
    /// crate is a bit-exact port of it; the test is here so the choice is on
    /// the record and an accidental change to `try_from` fails loudly rather
    /// than silently moving fight outcomes.
    #[test]
    fn cell_ids_outside_int_alias_through_the_java_narrowing() {
        let mut state = one_leek();
        // i64::MIN narrows to 0 → cell 0, whose AI-visible x is `0 - 18 + 1`.
        let got = call_official_builtin(&mut state, 0, "getCellX", &[Value::Int(i64::MIN)]);
        assert_value(&got, &Value::Int(-17), "getCellX(i64::MIN) aliases cell 0");
        // 2^32 + 5 narrows to 5.
        let aliased =
            call_official_builtin(&mut state, 0, "getCellY", &[Value::Int(4_294_967_296 + 5)]);
        let direct = call_official_builtin(&mut state, 0, "getCellY", &[Value::Int(5)]);
        assert_value(&aliased, &direct, "2^32 + 5 must answer exactly as 5 does");
    }

    /// A movement budget narrows the same way, and the same caveat applies.
    ///
    /// `pm_to_use as i32` truncates before the `pm > mp` clamp, so
    /// `moveToward(e, 4294967296)` spends no MP at all and
    /// `moveToward(e, 4294967297)` takes exactly one step. Saturating
    /// instead would read better — "a budget bigger than my MP means all of
    /// it" — but it is a different answer from the reference engine's
    /// `(int)` cast, and this crate is a bit-exact port whose consumers
    /// (`leek-generator`, `leek-scenario`) compare fight transcripts. Pinned
    /// here so the trade-off is visible and deliberate.
    #[test]
    fn a_movement_budget_narrows_like_a_java_int_cast() {
        let cases = [
            (-1_i64, 5),        // the documented "all my MP"
            (5, 5),             // exactly my MP
            (2, 2),             // less than my MP
            (4_294_967_296, 0), // narrows to 0 → no movement
            (4_294_967_297, 1), // narrows to 1 → one step
            (i64::MIN, 0),      // narrows to 0
            (-2, 0),            // a negative budget never moves
        ];
        for (budget, want) in cases {
            let mut state = one_leek();
            let moved = call_official_builtin(
                &mut state,
                0,
                "moveTowardCell",
                &[Value::Int(FAR_CELL), Value::Int(budget)],
            );
            assert_value(
                &moved,
                &Value::Int(want),
                &format!("moveTowardCell({FAR_CELL}, {budget})"),
            );
        }
    }

    /// The board's first cell — a corner, far more than 5 MP from
    /// `one_leek`'s mid-board start, so the budget always runs out before
    /// the path does and "how much MP did it spend" is the answer under test.
    const FAR_CELL: i64 = 0;
}
