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
    ChipSpec, ERROR_HELP_PAGE_LINK, FARMER_LOG_BULB_WITHOUT_AI, LOG_SSTANDARD, LOG_SWARNING, State,
    USE_RESURRECT_INVALID_ENTITY,
};

/// Dispatch one official fight function for the entity `current` (the fid
/// the running AI controls — `ai.getEntity()`).
#[must_use]
// Cell/coordinate args truncate through `(int)` in Java — `as i32` mirrors it.
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
    // an action in Java either.
    if state.fighters[current].is_dead() {
        return Value::Null;
    }

    let int_arg = |i: usize| args.get(i).map_or(0, Value::to_long);

    match name {
        // ---- FightClass ----
        "getTurn" => Value::Int(i64::from(state.order.turn())),
        "getNearestEnemy" => Value::Int(nearest_enemy(state, current)),
        "moveToward" => {
            // moveToward(leek_id[, pm_to_use]) — pm defaults to -1 (all MP).
            let pm = args.get(1).map_or(-1, Value::to_long);
            Value::Int(state.move_toward(current, int_arg(0), pm))
        }
        "moveTowardCell" => {
            // moveTowardCell(cell_id[, pm_to_use]) — pm defaults to the
            // entity's MP (the Java overload passes getMP() explicitly;
            // the state clamps to MP either way).
            let pm = args.get(1).map_or(-1, Value::to_long);
            Value::Int(state.move_toward_cell(current, int_arg(0), pm))
        }

        // ---- FieldClass ----
        // The AI-visible x axis is shifted: `getCellFromXY(x, y)` looks up
        // `(x + width - 1, y)` and `getCellX` shifts back.
        "getCellFromXY" => state
            .map
            .get_cell_xy(int_arg(0) as i32 + state.map.width - 1, int_arg(1) as i32)
            .map_or(Value::Null, |c| Value::Int(c as i64)),
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
        "setWeapon" => Value::Bool(set_weapon(state, current, int_arg(0))),
        // isStatic([entity]) — no arg (or null) means self; a non-entity
        // argument is false (Java's `instanceof Number` + lookup-miss paths).
        "isStatic" => Value::Bool(is_static(state, current, args.first())),

        // ---- WeaponClass ----
        "useWeapon" => Value::Int(use_weapon(state, current, int_arg(0))),

        // ---- ChipClass ----
        "useChip" => {
            // useChip(chip_id[, leek_id]) — the target defaults to self.
            #[allow(clippy::cast_possible_wrap)]
            let target = args.get(1).map_or(current as i64, Value::to_long);
            Value::Int(use_chip(state, current, int_arg(0), target))
        }
        "useChipOnCell" => {
            // useChipOnCell(chip_id, cell_id) — equipped chip + valid cell,
            // straight to `State.useChip` at that cell.
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
