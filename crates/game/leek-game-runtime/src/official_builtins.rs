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

use crate::state::State;

/// Dispatch one official fight function for the entity `current` (the fid
/// the running AI controls — `ai.getEntity()`).
#[must_use]
#[allow(clippy::cast_possible_wrap)]
pub fn call_official_builtin(
    state: &mut State,
    current: usize,
    name: &str,
    args: &[Value],
) -> Value {
    let int_arg = |i: usize| args.get(i).map_or(0, Value::to_long);

    match name {
        // ---- FightClass ----
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

        // ---- EntityClass ----
        "setWeapon" => Value::Bool(set_weapon(state, current, int_arg(0))),

        // ---- WeaponClass ----
        "useWeapon" => Value::Int(use_weapon(state, current, int_arg(0))),

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
