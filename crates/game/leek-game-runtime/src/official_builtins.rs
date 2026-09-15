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
use crate::builtins::message_text;
use crate::state::{
    CELL_EMPTY, CELL_ENTITY, CELL_OBSTACLE, ChipSpec, ERROR_HELP_PAGE_LINK,
    FARMER_LOG_ACTION_DENIED_IN_HOOK, FARMER_LOG_BULB_WITHOUT_AI,
    FARMER_LOG_LOADOUT_FORGOTTEN_ALREADY_EQUIPPED, FARMER_LOG_LOADOUT_NOT_FOUND,
    FARMER_LOG_SET_LOADOUT_NO_RESTAT_POTION, FARMER_LOG_SET_LOADOUT_OUT_OF_HOOK, Fighter,
    LOG_SSTANDARD, LOG_SWARNING, STAT_ABSOLUTE_SHIELD, STAT_AGILITY, STAT_DAMAGE_RETURN,
    STAT_MAGIC, STAT_POWER, STAT_RELATIVE_SHIELD, STAT_RESISTANCE, STAT_SCIENCE, STAT_STRENGTH,
    STAT_WISDOM, State, USE_RESURRECT_INVALID_ENTITY,
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
        // `FightClass.getNearestAlly` — `getNearestEnemy`'s twin, over the
        // caller's own team and skipping the caller itself. Same squared
        // Euclidean metric, same first-seen tie rule, same `-1` sentinel.
        "getNearestAlly" => Value::Int(nearest_ally(state, current)),
        // ---- FightClass (entity lists) ----
        // Every list here is built by `State.getAllEntities` /
        // `getTeamEntities` / `getEnemiesEntities`, which iterate the teams by
        // index and each `Team.mEntities` in insertion order — so a summon
        // comes right after the entity that summoned it (`State.summonEntity`
        // appends to its owner's team) and summons are *in* every one of these
        // lists.
        //
        // `getEnemies`/`getAllies` and their counts pass `get_deads = true`, so
        // dead entities are included; and Java filters the caller out of
        // neither, so `getAllies()` contains the caller. (`getAliveAllies`,
        // `getDeadAllies`, `getAliveEnemies` and `getDeadEnemies` are the
        // filtered variants; they are a later slice.)
        "getEnemies" => {
            let my_team = state.fighters[current].team;
            int_array(team_members(state, true, |t| t != my_team).into_iter())
        }
        "getAllies" => {
            let my_team = state.fighters[current].team;
            int_array(team_members(state, true, |t| t == my_team).into_iter())
        }
        "getEnemiesCount" => {
            let my_team = state.fighters[current].team;
            Value::Int(team_members(state, true, |t| t != my_team).len() as i64)
        }
        "getAlliesCount" => {
            let my_team = state.fighters[current].team;
            Value::Int(team_members(state, true, |t| t == my_team).len() as i64)
        }
        // Neither of these two is a reference-engine function: `FightFunctions`
        // registers no `getEntities` and no `getAliveEntities`, and neither
        // appears in `leekwars.library`. `getEntities` *is* in this toolchain's
        // resolver catalog and `builtins.rs` answers both, so — like
        // `getMaxLife` and `getTeam` below — they are served here with exactly
        // the meaning that engine gives them (`State.getAllEntities(true)` and
        // `getAllEntities(false)`), keeping the two engines in agreement.
        "getEntities" => int_array(team_members(state, true, |_| true).into_iter()),
        "getAliveEntities" => int_array(team_members(state, false, |_| true).into_iter()),
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
        "moveAwayFrom" => {
            // moveAwayFrom(leek_id[, pm_to_use]) — pm defaults to -1 (all MP).
            //
            // Note the asymmetry with `moveToward` right above: that one
            // charges `ai.ops(2000)` and this one charges nothing. The
            // dispatcher has no ops accounting at all yet, so neither is
            // modelled here — recorded so the gap is a known one rather than
            // a transcription slip.
            if deny_during_hook(state, current, "moveAwayFrom") {
                return Value::Int(0);
            }
            let pm = args.get(1).map_or(-1, Value::to_long);
            Value::Int(state.move_away_from(current, int_arg(0), pm))
        }
        "moveAwayFromCell" => {
            // moveAwayFromCell(cell_id[, pm_to_use]) — pm defaults to -1
            // (all MP); unlike `moveTowardCell` the reference's 1-arg
            // overload passes `-1` rather than `getMP()`, which comes to the
            // same thing after the clamp.
            if deny_during_hook(state, current, "moveAwayFromCell") {
                return Value::Int(0);
            }
            let pm = args.get(1).map_or(-1, Value::to_long);
            Value::Int(state.move_away_from_cell(current, int_arg(0), pm))
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
        // `Map.getObstacles()` — every non-walkable cell in cell-id order,
        // lazily cached on the map (hence the `&mut`).
        "getObstacles" => {
            let obstacles: Vec<i64> = state.map.obstacles().iter().map(|&c| c as i64).collect();
            int_array(obstacles.into_iter())
        }
        // `!walkable ? 2 : (player != null ? 1 : 0)`, and `-1` — not null —
        // for a cell that doesn't exist. Keep the order: an obstacle answers
        // `CELL_OBSTACLE` whatever else the map says about it, and
        // `CELL_ENTITY` and its deprecated `CELL_PLAYER` alias are both 1.
        "getCellContent" => state
            .map
            .get_cell(int_arg(0) as i32)
            .map_or(Value::Int(-1), |c| {
                Value::Int(if !state.map.cells[c].walkable {
                    CELL_OBSTACLE
                } else if state.entity_on(c).is_some() {
                    CELL_ENTITY
                } else {
                    CELL_EMPTY
                })
            }),
        // The four two-cell functions all answer their own sentinel when
        // either argument is off the board: `-1` for the two distances,
        // `false` for `isOnSameLine`, `null` for `lineOfSight`.
        "getCellDistance" => Value::Int(
            cell_pair(state, int_arg(0), int_arg(1))
                .map_or(-1, |(a, b)| i64::from(state.map.get_cell_distance(a, b))),
        ),
        // `Map.getDistance` is `sqrt(getDistance2(..))` — a `double`, and the
        // off-board sentinel is a `double` -1 too.
        "getDistance" => Value::Real(
            cell_pair(state, int_arg(0), int_arg(1))
                .map_or(-1.0, |(a, b)| state.map.get_euclidean_distance(a, b)),
        ),
        "isOnSameLine" => Value::Bool(
            cell_pair(state, int_arg(0), int_arg(1)).is_some_and(|(a, b)| state.map.in_line(a, b)),
        ),
        // `verifyLoS(s, e, null, cells)` — a *null* attack is `needLos = true`
        // there, so the LOS is always actually traced. The ignored-cell list
        // is the interesting half; see [`los_ignored_cells`].
        "lineOfSight" => match cell_pair(state, int_arg(0), int_arg(1)) {
            Some((a, b)) => {
                let ignored = los_ignored_cells(state, current, args.get(2));
                Value::Bool(state.map.verify_los(a, b, true, &ignored))
            }
            None => Value::Null,
        },
        // `getPath`/`getPathLength` share a shape: null for an off-board
        // endpoint, the empty answer when the two cells are the same (checked
        // *before* the A*, which reports no path at all for `start == end`),
        // then `Map.getPathBetween` — null again when the target is walled off.
        // Java also charges `distance² × 20` operations here; this dispatcher
        // models no operation budget at all, so that is not ported.
        "getPath" => match cell_pair(state, int_arg(0), int_arg(1)) {
            None => Value::Null,
            Some((a, b)) if a == b => int_array(std::iter::empty()),
            Some((a, b)) => {
                let ignored = path_ignored_cells(state, args.get(2));
                state
                    .map
                    .get_path_between(a, b, &ignored)
                    .map_or(Value::Null, |path| {
                        int_array(path.into_iter().map(|c| c as i64))
                    })
            }
        },
        "getPathLength" => match cell_pair(state, int_arg(0), int_arg(1)) {
            None => Value::Null,
            Some((a, b)) if a == b => Value::Int(0),
            Some((a, b)) => {
                let ignored = path_ignored_cells(state, args.get(2));
                state
                    .map
                    .get_path_between(a, b, &ignored)
                    .map_or(Value::Null, |path| Value::Int(path.len() as i64))
            }
        },

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

        // ---- EntityClass (characteristics, equipment) ----
        // Everything in this block resolves its optional entity argument
        // through `resolve_stat_target`, not `resolve_entity`:
        // `EntityClass.resolveStatTarget` masks *other* entities for the
        // duration of a `beforeFight()` hook, so the second AI to execute
        // can't read the first one's `setLoadout` choice. A masked or
        // unresolvable target answers `null`.
        //
        // `Entity.getStat(id)` is `mBaseStats + mBuffStats`, so every
        // characteristic below is the entity's *buffed* value, never the
        // scenario one.
        "getLife" => masked_int(state, current, args.first(), |f| i64::from(f.life)),
        // `Entity.getTotalLife()` is `mTotalLife` — the live maximum, which
        // vitality raises and erosion lowers — and NOT the `STAT_LIFE`
        // characteristic. `getMaxLife` is not a function of the reference
        // engine at all (`FightFunctions` registers `getTotalLife` and
        // nothing else), but it is a name this toolchain's resolver accepts
        // and `builtins.rs` already answers with the same value, so serving
        // it as an alias keeps the two engines agreeing.
        "getTotalLife" | "getMaxLife" => {
            masked_int(state, current, args.first(), |f| i64::from(f.total_life))
        }
        // Remaining TP/MP (`getTotalTP() - usedTP`), not the characteristic.
        "getTP" => masked_int(state, current, args.first(), |f| i64::from(f.tp())),
        "getMP" => masked_int(state, current, args.first(), |f| i64::from(f.mp())),
        "getStrength" => stat_of(state, current, args.first(), STAT_STRENGTH),
        "getAgility" => stat_of(state, current, args.first(), STAT_AGILITY),
        "getWisdom" => stat_of(state, current, args.first(), STAT_WISDOM),
        "getResistance" => stat_of(state, current, args.first(), STAT_RESISTANCE),
        "getScience" => stat_of(state, current, args.first(), STAT_SCIENCE),
        "getMagic" => stat_of(state, current, args.first(), STAT_MAGIC),
        "getPower" => stat_of(state, current, args.first(), STAT_POWER),
        "getAbsoluteShield" => stat_of(state, current, args.first(), STAT_ABSOLUTE_SHIELD),
        "getRelativeShield" => stat_of(state, current, args.first(), STAT_RELATIVE_SHIELD),
        "getDamageReturn" => stat_of(state, current, args.first(), STAT_DAMAGE_RETURN),
        // The *equipped* weapon (`Entity.weapon`) — `null` when the entity
        // carries none, which is every entity until its first `setWeapon`.
        "getWeapon" => match resolve_stat_target(state, current, args.first()) {
            Some(fid) => state.fighters[fid]
                .weapon
                .map_or(Value::Null, |w| Value::Int(i64::from(w))),
            None => Value::Null,
        },
        // Everything owned, in `mWeapons` insertion order (a `List`, not
        // sorted) …
        "getWeapons" => match resolve_stat_target(state, current, args.first()) {
            Some(fid) => int_array(state.fighters[fid].weapons.iter().map(|&w| i64::from(w))),
            None => Value::Null,
        },
        // … whereas `mChips` is a `TreeMap<Integer, Chip>`, so `getChips()`
        // comes back ordered by chip id — which is what iterating our
        // `BTreeSet` gives.
        "getChips" => match resolve_stat_target(state, current, args.first()) {
            Some(fid) => int_array(state.fighters[fid].chips.iter().map(|&c| i64::from(c))),
            None => Value::Null,
        },

        // ---- EntityClass (lobby-visible facts) ----
        // These resolve with a bare `Fight.getEntity` and are deliberately
        // NOT masked during `beforeFight()`: they expose nothing that isn't
        // already on the fight's lobby page or fixed at fight init.
        "getName" => match resolve_entity(state, current, args.first()) {
            Some(fid) => Value::String(std::rc::Rc::new(state.fighters[fid].name.clone())),
            None => Value::Null,
        },
        "getLevel" => match resolve_entity(state, current, args.first()) {
            Some(fid) => Value::Int(i64::from(state.fighters[fid].level)),
            None => Value::Null,
        },
        // The reference engine has no `getTeam`. It spells this one
        // `getSide` — `Entity.getTeam()`, the 0-based team *index* — and
        // keeps the real team id under `getTeamID` (`Entity.getTeamId()`).
        // `getTeam` is another resolver-only name, and `builtins.rs` already
        // answers it with the team index, so that is what it answers here.
        "getTeam" => match resolve_entity(state, current, args.first()) {
            Some(fid) => Value::Int(state.fighters[fid].team as i64),
            None => Value::Null,
        },
        // `isAlive`/`isDead` take a *required* entity and answer `false` —
        // not `null` — for one that doesn't resolve. Both are false for a
        // bogus id: `isDead(99999)` is not `true`.
        "isAlive" => Value::Bool(
            resolve_entity(state, current, args.first())
                .is_some_and(|fid| !state.fighters[fid].is_dead()),
        ),
        "isDead" => Value::Bool(
            resolve_entity(state, current, args.first())
                .is_some_and(|fid| state.fighters[fid].is_dead()),
        ),

        // ---- WeaponClass ----
        "useWeapon" => {
            if deny_during_hook(state, current, "useWeapon") {
                return Value::Int(-1);
            }
            Value::Int(use_weapon(state, current, int_arg(0)))
        }

        // ---- ChipClass ----
        // getCooldown(chip_id[, entity]) — the *chip* comes first, the same
        // argument order `builtins.rs` uses. `State.getCooldown` reads the
        // team's table for a team-cooldown chip and the entity's own
        // otherwise, and answers 0 for a chip the catalog doesn't know
        // (`Chips.getChip` returns null there). Not masked: `ChipClass`
        // resolves with a bare `Fight.getEntity`.
        "getCooldown" => match resolve_entity(state, current, args.get(1)) {
            Some(fid) => Value::Int(i64::from(state.chip_cooldown(fid, int_arg(0) as i32))),
            None => Value::Null,
        },
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

        // ---- EntityClass (communication) ----
        // `say(message)` — 1 TP, at most `SAY_LIMIT_TURN` logged per turn.
        // Deliberately *not* behind `deny_during_hook`: `EntityClass.say`
        // carries no `denyDuringHook` call, so talking from `beforeFight()`
        // or `afterFight()` is legal and logs normally.
        //
        // Java returns a real boolean here — `false` when the entity is out
        // of TP or has already used its says for the turn — so this arm
        // forwards `State::say`'s answer rather than the unconditional
        // `true` `builtins.rs` gives.
        "say" => Value::Bool(state.say(current, &message_text(args.first()))),

        // ---- UtilClass (debug marks) ----
        // `mark`, `markText` and `clearMarks` write to `ai.getLogs()` —
        // `LeekLog.addCell`/`addCellText`/`addClearCells`, which land as
        // `MARK`/`MARK_TEXT`/`CLEAR_CELLS` entries in the calling *farmer's*
        // private debug-log stream, the same channel as `debug()`. They do
        // NOT go through `Fight.log`, so no `Action` of any kind reaches the
        // report and no fight transcript can observe them. (`show(cell)` is
        // the one that logs `ActionShowCell` under the `showsTurn` cap; it is
        // a different function and not part of this batch.)
        //
        // `State` models the farmer log only as the keyed system-log table
        // (`add_system_log`), which has no room for a mark payload, so the
        // payload stays unmodelled — as it already is in `builtins.rs`. What
        // *is* ported is the return value, which an AI can branch on: a mark
        // answers whether it had at least one real cell to mark.
        "mark" => Value::Bool(marked_cell_count(state, args.first()) > 0),
        // `markText` additionally accepts a `map<cell, text>`, and answers
        // `true` for one unconditionally — even an empty map, whose `for`
        // loop marks nothing and still falls through to `return true`.
        "markText" => Value::Bool(match args.first() {
            Some(Value::Map(_)) => true,
            other => marked_cell_count(state, other) > 0,
        }),
        // `UtilClass.clearMarks` is declared `Type.VOID` and returns `null`.
        "clearMarks" => Value::Null,

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

/// `FightClass.getNearestAlly` — [`nearest_enemy`] over the caller's own team,
/// skipping the caller itself (`l == ai.getEntity()`). Same squared Euclidean
/// metric, same `d < dist || dist == -1` first-seen tie rule, same `-1`.
#[allow(clippy::cast_possible_wrap)]
fn nearest_ally(state: &State, current: usize) -> i64 {
    let Some(my_cell) = state.fighters[current].cell else {
        return -1;
    };
    let my_team = state.fighters[current].team;
    let mut dist = -1;
    let mut nearest = -1;
    for &fid in &state.teams[my_team].fighters {
        if fid == current {
            continue;
        }
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
    nearest
}

/// `State.getAllEntities` / `getTeamEntities` / `getEnemiesEntities` — the fids
/// of every entity on a team `keep` selects.
///
/// The order is the reference engine's: teams by index, then each team's
/// `mEntities` in insertion order. Summons are in these lists (Java's
/// `State.summonEntity` does `teams.get(team).addEntity(invoc)`), right after
/// the entity that summoned them. `with_dead` is Java's `get_deads` flag.
#[allow(clippy::cast_possible_wrap)]
fn team_members(state: &State, with_dead: bool, keep: impl Fn(usize) -> bool) -> Vec<i64> {
    let mut fids = Vec::new();
    for (t, team) in state.teams.iter().enumerate() {
        if !keep(t) {
            continue;
        }
        for &fid in &team.fighters {
            if with_dead || !state.fighters[fid].is_dead() {
                fids.push(fid as i64);
            }
        }
    }
    fids
}

/// The two cells a `FieldClass` function's arguments name, or `None` when
/// either is off the board — the branch every one of them takes to its own
/// sentinel. The `as i32` is the file-wide `(int)` narrowing documented on
/// [`call_official_builtin`]; `Map::get_cell` range-checks what comes out.
#[allow(clippy::cast_possible_truncation)]
fn cell_pair(state: &State, a: i64, b: i64) -> Option<(usize, usize)> {
    Some((state.map.get_cell(a as i32)?, state.map.get_cell(b as i32)?))
}

/// Java's `value instanceof Number` over a LeekScript value — what the
/// `lineOfSight` and `getPath` ignore arguments branch on. `BigIntegerValue`
/// extends `Number`, so a big integer takes the numeric branch too.
fn is_number(v: &Value) -> bool {
    matches!(v, Value::Int(_) | Value::Real(_) | Value::BigInt(_))
}

/// `ai.getFight().getEntity(ai.integer(v))`, then its cell. `ai.integer` is a
/// Java `(int)` cast, so the same truncation as everywhere else in this file.
#[allow(clippy::cast_possible_truncation)]
fn ignored_entity_cell(state: &State, id: i64) -> Option<usize> {
    let fid = usize::try_from(id as i32).ok()?;
    state.fighters.get(fid)?.cell
}

/// `FieldClass.lineOfSight`'s ignored-cell list.
///
/// The three Java branches are deliberately **not** symmetric, and the
/// asymmetry is observable:
///
/// * a *number* is one **entity** id, and its cell is the whole list — the
///   caller's own cell is NOT ignored on this path;
/// * an *array* is a list of **entity** ids, each resolved to its cell, and
///   the caller's own cell is prepended;
/// * anything else — including the 2-argument form, which delegates with
///   `ignore = null` — ignores exactly the caller's own cell.
///
/// Java's last branch adds `ai.getEntity().getCell()` unconditionally, so a
/// cell-less (dead) caller pushes a `null` that `List.contains` can never
/// match; skipping it here is the same answer.
fn los_ignored_cells(state: &State, current: usize, arg: Option<&Value>) -> Vec<usize> {
    let mut cells = Vec::new();
    match arg {
        Some(v) if is_number(v) => {
            cells.extend(ignored_entity_cell(state, v.to_long()));
        }
        Some(Value::Array(a)) => {
            cells.extend(state.fighters[current].cell);
            for v in a.borrow().iter() {
                if is_number(v) {
                    cells.extend(ignored_entity_cell(state, v.to_long()));
                }
            }
        }
        _ => cells.extend(state.fighters[current].cell),
    }
    cells
}

/// `FieldClass.getPath`/`getPathLength`'s ignored-cell list — and note that it
/// reads its array the *other* way round from [`los_ignored_cells`]: despite
/// the `leeks_to_ignore` parameter name, `EntityAI.putCells` resolves each
/// element as a **cell** id, dropping the ones that aren't on the board, and
/// it never adds the caller's own cell.
///
/// The number form is the deprecated `getPath(start, end, leek_to_ignore)`
/// overload, which does take an entity. Java also emits a free-text
/// `AILog.WARNING` there; this crate has no free-text AI-log channel (only
/// keyed system logs), so that one log line is unported — the returned path is
/// the same.
#[allow(clippy::cast_possible_truncation)]
fn path_ignored_cells(state: &State, arg: Option<&Value>) -> Vec<usize> {
    match arg {
        Some(Value::Array(a)) => a
            .borrow()
            .iter()
            .filter_map(|v| state.map.get_cell(v.to_long() as i32))
            .collect(),
        Some(v) if is_number(v) => ignored_entity_cell(state, v.to_long())
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
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

/// How many real board cells a `mark`/`markText` argument names —
/// `UtilClass.mark`'s `cel` array, whose emptiness is the whole difference
/// between the function's `true` and its `false`.
///
/// A number is one candidate (`ai.integer` narrows it with Java's `(int)`
/// cast first, so an id outside `int` aliases rather than missing); an array
/// contributes each of its elements that resolves, silently dropping the
/// rest; anything else — a string, a boolean, `null`, a missing argument —
/// is not a cell argument at all and contributes nothing.
// `as i32` is `AI.integer`'s own `(int)` cast, the narrowing the fn doc
// above describes; `Map::get_cell` range-checks whatever comes out of it.
#[allow(clippy::cast_possible_truncation)]
fn marked_cell_count(state: &State, arg: Option<&Value>) -> usize {
    let resolves = |v: &Value| state.map.get_cell(v.to_long() as i32).is_some();
    match arg {
        // `cell instanceof Number` — `Long`, `Double` and `BigInteger` all
        // are; `Boolean` is not.
        Some(v @ (Value::Int(_) | Value::Real(_) | Value::BigInt(_))) => usize::from(resolves(v)),
        Some(Value::Array(a)) => a.borrow().iter().filter(|v| resolves(v)).count(),
        _ => 0,
    }
}

/// `EntityClass.resolveStatTarget` — [`resolve_entity`] plus the
/// `beforeFight()` mask: while that hook runs, an entity other than the
/// caller answers `null` for every equipment- and stat-dependent getter, so
/// the second AI to execute can't react to the first one's `setLoadout`
/// choice. Self queries and the `afterFight()` hook are unmasked.
fn resolve_stat_target(state: &State, current: usize, arg: Option<&Value>) -> Option<usize> {
    let fid = resolve_entity(state, current, arg)?;
    if state.is_in_before_fight_hook() && fid != current {
        return None;
    }
    Some(fid)
}

/// One masked `long` getter: resolve through [`resolve_stat_target`], then
/// read `f` off the fighter. A masked or unresolvable target is `null`.
fn masked_int(
    state: &State,
    current: usize,
    arg: Option<&Value>,
    f: impl FnOnce(&Fighter) -> i64,
) -> Value {
    match resolve_stat_target(state, current, arg) {
        Some(fid) => Value::Int(f(&state.fighters[fid])),
        None => Value::Null,
    }
}

/// One masked characteristic (`Entity.getStat` — `mBaseStats + mBuffStats`).
fn stat_of(state: &State, current: usize, arg: Option<&Value>, stat: usize) -> Value {
    masked_int(state, current, arg, |f| i64::from(f.stat(stat)))
}

/// A LeekScript array of ids — the shape every array-returning getter here
/// builds (`new ArrayLeekValue(ai)` then one `push` per element).
fn int_array(ids: impl Iterator<Item = i64>) -> Value {
    Value::Array(std::rc::Rc::new(std::cell::RefCell::new(
        ids.map(Value::Int).collect(),
    )))
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
    int_array(
        team.fighters
            .iter()
            .filter(|&&f| !state.fighters[f].is_dead() && state.fighters[f].summoner == Some(fid))
            .map(|&f| f as i64),
    )
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

    use super::{call_official_builtin, int_array};
    use crate::state::{
        CELL_EMPTY, CELL_ENTITY, CELL_OBSTACLE, Fighter, HookPhase, STAT_ABSOLUTE_SHIELD,
        STAT_AGILITY, STAT_DAMAGE_RETURN, STAT_LIFE, STAT_MAGIC, STAT_MP, STAT_POWER,
        STAT_RELATIVE_SHIELD, STAT_RESISTANCE, STAT_SCIENCE, STAT_STRENGTH, STAT_TP, STAT_WISDOM,
        State, Stats,
    };

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
            "moveAwayFrom",
            "moveAwayFromCell",
            "say",
            "mark",
            "markText",
            "clearMarks",
            "useChip",
            "useChipOnCell",
            "useWeapon",
            "setWeapon",
            "summon",
            "getCellToUseChip",
            "isStatic",
            "getEnemies",
            "getAllies",
            "getEnemiesCount",
            "getAlliesCount",
            "getEntities",
            "getAliveEntities",
            "getNearestAlly",
            "getObstacles",
            "getCellContent",
            "getCellDistance",
            "getDistance",
            "isOnSameLine",
            "lineOfSight",
            "getPath",
            "getPathLength",
            "getLife",
            "getTotalLife",
            "getMaxLife",
            "getTP",
            "getMP",
            "getStrength",
            "getAgility",
            "getWisdom",
            "getResistance",
            "getScience",
            "getMagic",
            "getPower",
            "getAbsoluteShield",
            "getRelativeShield",
            "getDamageReturn",
            "getWeapon",
            "getWeapons",
            "getChips",
            "getName",
            "getLevel",
            "getTeam",
            "isAlive",
            "isDead",
            "getCooldown",
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

    /// Two leeks on opposing teams, every characteristic a different number
    /// so a getter wired to the wrong `STAT_*` fails loudly rather than
    /// matching by coincidence. Fighter 0 is the one the calls run as; it
    /// carries a buff, spent TP/MP, damage and a vitality bonus, so no
    /// getter here can be satisfied by the raw scenario stats. Fighter 1 is
    /// dead, which is what `isDead`/`isAlive` answer about.
    fn two_leeks() -> State {
        let mut stats = Stats::default();
        stats.set(STAT_LIFE, 100);
        stats.set(STAT_TP, 10);
        stats.set(STAT_MP, 5);
        stats.set(STAT_STRENGTH, 11);
        stats.set(STAT_AGILITY, 12);
        stats.set(STAT_WISDOM, 13);
        stats.set(STAT_RESISTANCE, 14);
        stats.set(STAT_SCIENCE, 15);
        stats.set(STAT_MAGIC, 16);
        stats.set(STAT_POWER, 17);
        stats.set(STAT_ABSOLUTE_SHIELD, 18);
        stats.set(STAT_RELATIVE_SHIELD, 19);
        stats.set(STAT_DAMAGE_RETURN, 20);

        let mut state = State::new(42);
        let me = state.add_entity(0, Fighter::new(0, 1, "mine".into(), 0, stats.clone()));
        state.place_entity(me, 306);
        let them = state.add_entity(1, Fighter::new(0, 2, "theirs".into(), 1, stats));
        state.place_entity(them, 300);

        state.fighters[me].level = 42;
        // Spent TP/MP: `getTP`/`getMP` are the remainder, not the stat.
        state.fighters[me].use_tp(3);
        state.fighters[me].use_mp(2);
        // A buff: `Entity.getStat` is base + buff, so 11 + 100.
        state.fighters[me].buff_stats.set(STAT_STRENGTH, 100);
        // Damaged, and vitality-boosted past the `STAT_LIFE` characteristic:
        // `getLife`, `getTotalLife` and `STAT_LIFE` are three numbers.
        state.fighters[me].life = 60;
        state.fighters[me].total_life = 130;
        // `mWeapons` is a `List` — insertion order, deliberately unsorted.
        state.fighters[me].weapons = vec![37, 1, 19];
        state.fighters[me].weapon = Some(19);
        // `mChips` is a `TreeMap` — inserted unsorted, read back by id.
        state.fighters[me].chips = [7, 2, 3].into_iter().collect();
        state.fighters[them].life = 0;
        state
    }

    /// Dispatch as fighter 0 of a [`two_leeks`] state.
    fn call(state: &mut State, name: &str, args: &[Value]) -> Value {
        call_official_builtin(state, 0, name, args)
    }

    /// The ids inside an array `Value`, which `identity_eq` can't compare
    /// (it is pointer equality for arrays).
    #[track_caller]
    fn int_vec(v: &Value) -> Vec<i64> {
        let Value::Array(a) = v else {
            panic!("expected an array, got {v:?}");
        };
        a.borrow().iter().map(Value::to_long).collect()
    }

    /// Each entity getter reads the field the Java one reads.
    ///
    /// The fixture is built so that every expected number is unique: a
    /// getter pointed at the wrong `STAT_*`, at the base stats instead of
    /// the buffed ones, or at `STAT_LIFE` instead of `mTotalLife`, lands on
    /// a value no other getter answers and fails here.
    #[test]
    fn entity_getters_read_the_java_field() {
        let mut state = two_leeks();
        let cases: &[(&str, Value)] = &[
            ("getLife", Value::Int(60)),
            // `mTotalLife`, not `STAT_LIFE` (100) and not current life (60).
            ("getTotalLife", Value::Int(130)),
            // Not a reference-engine name; served as `getTotalLife`.
            ("getMaxLife", Value::Int(130)),
            ("getTP", Value::Int(7)),
            ("getMP", Value::Int(3)),
            // base 11 + buff 100 — `getStat`, not `mBaseStats`.
            ("getStrength", Value::Int(111)),
            ("getAgility", Value::Int(12)),
            ("getWisdom", Value::Int(13)),
            ("getResistance", Value::Int(14)),
            ("getScience", Value::Int(15)),
            ("getMagic", Value::Int(16)),
            ("getPower", Value::Int(17)),
            ("getAbsoluteShield", Value::Int(18)),
            ("getRelativeShield", Value::Int(19)),
            ("getDamageReturn", Value::Int(20)),
            ("getWeapon", Value::Int(19)),
            ("getLevel", Value::Int(42)),
            // The 0-based team *index*, as `getSide` answers upstream.
            ("getTeam", Value::Int(0)),
            ("getName", Value::String(std::rc::Rc::new("mine".into()))),
            ("isAlive", Value::Bool(true)),
            ("isDead", Value::Bool(false)),
        ];
        for (name, want) in cases {
            let got = call(&mut state, name, &[]);
            assert_value(&got, want, &format!("{name}()"));
            // The explicit-self form must answer identically.
            let explicit = call(&mut state, name, &[Value::Int(0)]);
            assert_value(&explicit, want, &format!("{name}(0)"));
        }
        // And the other entity is a different answer, so "self" isn't being
        // hard-coded: fighter 1 is dead and undamaged-at-death.
        assert_value(
            &call(&mut state, "isDead", &[Value::Int(1)]),
            &Value::Bool(true),
            "isDead(1)",
        );
        assert_value(
            &call(&mut state, "getName", &[Value::Int(1)]),
            &Value::String(std::rc::Rc::new("theirs".into())),
            "getName(1)",
        );
    }

    /// `getWeapons` keeps `mWeapons`' insertion order; `getChips` comes out
    /// of a `TreeMap`, so it is sorted by chip id whatever order the chips
    /// were equipped in.
    #[test]
    fn weapon_and_chip_arrays_keep_the_java_orders() {
        let mut state = two_leeks();
        assert_eq!(
            int_vec(&call(&mut state, "getWeapons", &[])),
            vec![37, 1, 19],
            "getWeapons() is mWeapons order, not sorted"
        );
        assert_eq!(
            int_vec(&call(&mut state, "getChips", &[])),
            vec![2, 3, 7],
            "getChips() is TreeMap order, i.e. by chip id"
        );
        // An entity with nothing equipped answers an empty array, not null.
        assert_eq!(
            int_vec(&call(&mut state, "getWeapons", &[Value::Int(1)])),
            Vec::<i64>::new(),
            "getWeapons(1)"
        );
    }

    /// `getCooldown(chip[, entity])` — chip first, and a team-cooldown chip
    /// reads the team's table rather than the entity's.
    #[test]
    fn get_cooldown_takes_the_chip_first() {
        let mut state = two_leeks();
        // 3 = bandage (a per-entity cooldown), 73 = puny bulb (a team one).
        for id in [3, 73] {
            let spec = crate::official_items::chip_spec(id).expect("catalog chip");
            state.chip_specs.insert(id, spec);
        }
        state.fighters[0].add_cooldown(3, 2);
        state.teams[0].add_cooldown(73, 4);

        assert_value(
            &call(&mut state, "getCooldown", &[Value::Int(3)]),
            &Value::Int(2),
            "getCooldown(3) reads the entity's table",
        );
        assert_value(
            &call(&mut state, "getCooldown", &[Value::Int(73)]),
            &Value::Int(4),
            "getCooldown(73) reads the team's table",
        );
        // The second argument is the entity — the enemy shares neither table.
        assert_value(
            &call(&mut state, "getCooldown", &[Value::Int(3), Value::Int(1)]),
            &Value::Int(0),
            "getCooldown(3, 1)",
        );
        assert_value(
            &call(&mut state, "getCooldown", &[Value::Int(73), Value::Int(1)]),
            &Value::Int(0),
            "getCooldown(73, 1)",
        );
        // A chip the catalog doesn't know is 0 (`Chips.getChip` → null), an
        // entity that doesn't resolve is null.
        assert_value(
            &call(&mut state, "getCooldown", &[Value::Int(9999)]),
            &Value::Int(0),
            "getCooldown(9999)",
        );
        assert_value(
            &call(&mut state, "getCooldown", &[Value::Int(3), Value::Int(99)]),
            &Value::Null,
            "getCooldown(3, 99)",
        );
    }

    /// `EntityClass.resolveStatTarget` masks *other* entities' stats and
    /// equipment while a `beforeFight()` hook runs, so the AI that executes
    /// second can't read what the first one's `setLoadout` picked. Self
    /// queries, lobby facts and the `afterFight()` hook stay readable.
    #[test]
    fn a_before_fight_hook_masks_another_entitys_stats() {
        let mut state = two_leeks();
        // Not `getWeapon`: fighter 1 has nothing equipped, so it is null
        // masked or not, and would prove nothing here.
        let readable = ["getAgility", "getWeapons", "getChips"];
        for name in readable {
            let got = call(&mut state, name, &[Value::Int(1)]);
            assert!(
                !matches!(got, Value::Null),
                "{name}(1) outside a hook must not be null, got {got:?}"
            );
        }
        state.hook_phase = HookPhase::BeforeFight;
        for name in readable {
            let got = call(&mut state, name, &[Value::Int(1)]);
            assert_value(&got, &Value::Null, &format!("{name}(1) in beforeFight"));
            // Self is never masked.
            let mine = call(&mut state, name, &[]);
            assert!(
                !matches!(mine, Value::Null),
                "{name}() in beforeFight is a self query, got {mine:?}"
            );
        }
        // Lobby facts are never masked, hook or not.
        assert_value(
            &call(&mut state, "getLevel", &[Value::Int(1)]),
            &Value::Int(1),
            "getLevel(1) in beforeFight",
        );
        assert_value(
            &call(&mut state, "getTeam", &[Value::Int(1)]),
            &Value::Int(1),
            "getTeam(1) in beforeFight",
        );
        // afterFight() unmasks again.
        state.hook_phase = HookPhase::AfterFight;
        assert_value(
            &call(&mut state, "getAgility", &[Value::Int(1)]),
            &Value::Int(12),
            "getAgility(1) in afterFight",
        );
    }

    /// An entity id that resolves to nothing is `null` for every getter —
    /// except `isAlive`/`isDead`, which take a required entity and answer
    /// `false` (both of them) rather than `null`.
    #[test]
    fn an_unresolvable_entity_is_null_but_never_dead() {
        let mut state = two_leeks();
        const NULLABLE: &[&str] = &[
            "getLife",
            "getTotalLife",
            "getMaxLife",
            "getTP",
            "getMP",
            "getStrength",
            "getAgility",
            "getWisdom",
            "getResistance",
            "getScience",
            "getMagic",
            "getPower",
            "getAbsoluteShield",
            "getRelativeShield",
            "getDamageReturn",
            "getWeapon",
            "getWeapons",
            "getChips",
            "getName",
            "getLevel",
            "getTeam",
        ];
        for name in NULLABLE {
            let got = call(&mut state, name, &[Value::Int(99)]);
            assert_value(&got, &Value::Null, &format!("{name}(99)"));
        }
        assert_value(
            &call(&mut state, "isAlive", &[Value::Int(99)]),
            &Value::Bool(false),
            "isAlive(99)",
        );
        assert_value(
            &call(&mut state, "isDead", &[Value::Int(99)]),
            &Value::Bool(false),
            "isDead(99) is false too — a missing entity is not a dead one",
        );
    }

    // ── Entity lists (FightClass) ────────────────────────────────────────────

    /// An index (fid or cell id) as the argument a builtin takes.
    fn id(n: usize) -> Value {
        Value::Int(i64::try_from(n).expect("an index fits in i64"))
    }

    /// Two teams of unequal size, filled **alternately** so that fid order and
    /// team-list order disagree: team 0 is `[0, 2, 4]` and team 1 is `[1, 3]`,
    /// while the fighter arena is `0, 1, 2, 3, 4`. A list built by walking
    /// `state.fighters` instead of `state.teams` comes out in the wrong order
    /// and fails here.
    ///
    /// Fid 2 is a dead ally (the `get_deads` flag is what decides whether it
    /// shows up), fid 3 is the enemy's summon (summons are in every one of
    /// these lists) and fid 4 is a live ally standing next to the caller.
    fn a_team_fight() -> State {
        let mut stats = Stats::default();
        stats.set(STAT_LIFE, 100);
        stats.set(STAT_TP, 10);
        stats.set(STAT_MP, 5);

        let mut state = State::new(42);
        let me = state.add_entity(0, Fighter::new(0, 1, "mine".into(), 0, stats.clone()));
        let enemy = state.add_entity(1, Fighter::new(0, 2, "theirs".into(), 1, stats.clone()));
        let dead_ally = state.add_entity(0, Fighter::new(0, 3, "ghost".into(), 0, stats.clone()));
        let summon = state.add_entity(1, Fighter::new(0, 4, "bulb".into(), 1, stats.clone()));
        let ally = state.add_entity(0, Fighter::new(0, 5, "friend".into(), 0, stats));

        state.place_entity(me, 306);
        state.place_entity(enemy, 300);
        state.place_entity(dead_ally, 0);
        state.place_entity(summon, 307);
        state.place_entity(ally, 289);

        state.fighters[dead_ally].life = 0;
        state.fighters[summon].summoner = Some(enemy);
        state
    }

    /// The four AI-visible lists, and the two resolver-only names, all come out
    /// in `State.getAllEntities` order — teams by index, then each team's
    /// members in insertion order.
    ///
    /// The three facts this pins that are easy to get wrong, all read off
    /// `FightClass.java`: `getAllies`/`getEnemies` pass `get_deads = true`, so
    /// the dead ally is in the list; neither filters the caller out, so
    /// `getAllies()` contains the caller itself; and summons are ordinary team
    /// members, so the enemy bulb is an enemy.
    #[test]
    fn entity_lists_follow_the_java_team_order() {
        let mut state = a_team_fight();
        assert_eq!(
            int_vec(&call(&mut state, "getAllies", &[])),
            vec![0, 2, 4],
            "getAllies() is team 0 with the dead ally AND the caller in it"
        );
        assert_eq!(
            int_vec(&call(&mut state, "getEnemies", &[])),
            vec![1, 3],
            "getEnemies() is team 1, the summon included"
        );
        assert_value(
            &call(&mut state, "getAlliesCount", &[]),
            &Value::Int(3),
            "getAlliesCount()",
        );
        assert_value(
            &call(&mut state, "getEnemiesCount", &[]),
            &Value::Int(2),
            "getEnemiesCount()",
        );
        // Teams first, arena order second: `[0, 2, 4]` then `[1, 3]`.
        assert_eq!(
            int_vec(&call(&mut state, "getEntities", &[])),
            vec![0, 2, 4, 1, 3],
            "getEntities() is getAllEntities(true)"
        );
        assert_eq!(
            int_vec(&call(&mut state, "getAliveEntities", &[])),
            vec![0, 4, 1, 3],
            "getAliveEntities() is getAllEntities(false) — fid 2 is dead"
        );
        // And the same lists read from the enemy's seat are the mirror image,
        // so "ally" isn't hard-coded to team 0.
        assert_eq!(
            int_vec(&call_official_builtin(&mut state, 1, "getAllies", &[])),
            vec![1, 3],
            "getAllies() as fighter 1"
        );
        assert_eq!(
            int_vec(&call_official_builtin(&mut state, 1, "getEnemies", &[])),
            vec![0, 2, 4],
            "getEnemies() as fighter 1"
        );
    }

    /// `getNearestAlly` is `getNearestEnemy`'s twin: same squared-Euclidean
    /// metric, same `-1` sentinel — but over the caller's own team, skipping
    /// the caller and every dead member.
    #[test]
    fn get_nearest_ally_skips_the_caller_and_the_dead() {
        let mut state = a_team_fight();
        // Fid 4 stands on 289, a neighbour of the caller's 306; fid 2 is a
        // corner away but dead, and fid 0 is the caller.
        assert_value(
            &call(&mut state, "getNearestAlly", &[]),
            &Value::Int(4),
            "getNearestAlly()",
        );
        // Kill the only live ally and the answer is the sentinel, not the
        // caller and not the corpse.
        state.fighters[4].life = 0;
        assert_value(
            &call(&mut state, "getNearestAlly", &[]),
            &Value::Int(-1),
            "getNearestAlly() with no live ally left",
        );
        // A lone leek has no ally either.
        let mut alone = one_leek();
        assert_value(
            &call_official_builtin(&mut alone, 0, "getNearestAlly", &[]),
            &Value::Int(-1),
            "getNearestAlly() with a one-entity team",
        );
    }

    // ── Field geometry (FieldClass) ──────────────────────────────────────────

    /// A 1v1 on a **generated** board — `State::init()` draws the obstacle
    /// count and runs `Map::generate_map`, exactly as a real fight does — so
    /// the pathfinding and obstacle tests run against a real board rather than
    /// an empty grid.
    fn generated_fight() -> State {
        let mut stats = Stats::default();
        stats.set(STAT_LIFE, 100);
        stats.set(STAT_TP, 10);
        stats.set(STAT_MP, 5);
        let mut state = State::new(7);
        state.add_entity(0, Fighter::new(0, 1, "mine".into(), 0, stats.clone()));
        state.add_entity(1, Fighter::new(0, 2, "theirs".into(), 1, stats));
        state.init();
        state
    }

    /// `getObstacles` is `Map.getObstacles()` — every non-walkable cell, in
    /// cell-id order.
    #[test]
    fn get_obstacles_lists_every_unwalkable_cell() {
        let mut state = generated_fight();
        let want: Vec<i64> = state
            .map
            .cells
            .iter()
            .filter(|c| !c.walkable)
            .map(|c| i64::try_from(c.id).expect("a cell id fits in i64"))
            .collect();
        assert!(!want.is_empty(), "a generated board has obstacles on it");
        assert_eq!(int_vec(&call(&mut state, "getObstacles", &[])), want);
    }

    /// `!walkable ? CELL_OBSTACLE : (entity ? CELL_ENTITY : CELL_EMPTY)`, and
    /// `-1` — a plain integer, not `null` — for a cell that isn't on the board.
    #[test]
    fn get_cell_content_answers_obstacle_entity_or_empty() {
        let mut state = generated_fight();
        let obstacle = state
            .map
            .cells
            .iter()
            .find(|c| !c.walkable)
            .expect("a generated board has an obstacle")
            .id;
        let occupied = state.fighters[0].cell.expect("init places both leeks");
        let empty = state
            .map
            .cells
            .iter()
            .find(|c| c.walkable && state.entity_on(c.id).is_none())
            .expect("a generated board has a free cell")
            .id;

        assert_value(
            &call(&mut state, "getCellContent", &[id(obstacle)]),
            &Value::Int(CELL_OBSTACLE),
            "getCellContent(an obstacle)",
        );
        assert_value(
            &call(&mut state, "getCellContent", &[id(occupied)]),
            &Value::Int(CELL_ENTITY),
            "getCellContent(an occupied cell)",
        );
        assert_value(
            &call(&mut state, "getCellContent", &[id(empty)]),
            &Value::Int(CELL_EMPTY),
            "getCellContent(a free cell)",
        );
        for off_board in [-1_i64, 613, 10_000] {
            assert_value(
                &call(&mut state, "getCellContent", &[Value::Int(off_board)]),
                &Value::Int(-1),
                &format!("getCellContent({off_board})"),
            );
        }
    }

    /// The two-cell measurements: `getCellDistance` is the Manhattan
    /// `Pathfinding.getCaseDistance`, `getDistance` the Euclidean
    /// `Map.getDistance`, `isOnSameLine` a shared-row-or-column test.
    #[test]
    fn two_cell_measurements_use_the_map_metrics() {
        let mut state = one_leek();
        // 288 = (16, 0), 306 = (17, 0), 324 = (18, 0) — one board row.
        assert_value(
            &call(&mut state, "getCellDistance", &[id(288), id(324)]),
            &Value::Int(2),
            "getCellDistance(288, 324)",
        );
        assert_value(
            &call(&mut state, "isOnSameLine", &[id(288), id(324)]),
            &Value::Bool(true),
            "isOnSameLine(288, 324)",
        );
        assert_value(
            &call(&mut state, "isOnSameLine", &[id(288), id(289)]),
            &Value::Bool(false),
            "isOnSameLine(288, 289) — neither row nor column is shared",
        );
        let got = call(&mut state, "getDistance", &[id(288), id(324)]);
        let Value::Real(d) = got else {
            panic!("getDistance must answer a real, got {got:?}");
        };
        assert!(
            (d - 2.0).abs() < 1e-9,
            "getDistance(288, 324) = {d}, expected 2"
        );
    }

    /// Off-board arguments take each two-cell function to its own sentinel:
    /// `-1` for the distances, `false` for `isOnSameLine`, `null` for
    /// `lineOfSight`, `getPath` and `getPathLength`.
    #[test]
    fn off_board_cells_take_each_field_function_to_its_sentinel() {
        let mut state = one_leek();
        for (a, b) in [(613_i64, 306_i64), (306, 613), (-1, -1)] {
            let args = [Value::Int(a), Value::Int(b)];
            assert_value(
                &call(&mut state, "getCellDistance", &args),
                &Value::Int(-1),
                &format!("getCellDistance({a}, {b})"),
            );
            let got = call(&mut state, "getDistance", &args);
            let Value::Real(d) = got else {
                panic!("getDistance must answer a real, got {got:?}");
            };
            assert!(
                (d + 1.0).abs() < 1e-9,
                "getDistance({a}, {b}) = {d}, expected -1"
            );
            assert_value(
                &call(&mut state, "isOnSameLine", &args),
                &Value::Bool(false),
                &format!("isOnSameLine({a}, {b})"),
            );
            for name in ["lineOfSight", "getPath", "getPathLength"] {
                assert_value(
                    &call(&mut state, name, &args),
                    &Value::Null,
                    &format!("{name}({a}, {b})"),
                );
            }
        }
    }

    /// `getPath` is the map's diamond A*, not a hand-rolled grid walk.
    ///
    /// The board here is **generated** — the same `Map::generate_map` a real
    /// fight draws, obstacles and all — and the expectation is recomputed from
    /// `Map::get_astar_path` rather than written out as a cell list, which on
    /// an obstacle-free grid would prove nothing about which pathfinder ran.
    #[test]
    fn get_path_is_the_maps_a_star_on_a_generated_map() {
        let mut state = generated_fight();
        let start = state.fighters[0].cell.expect("init places both leeks");
        let end = state.fighters[1].cell.expect("init places both leeks");
        let want: Vec<i64> = state
            .map
            .get_astar_path(start, &[end], &[])
            .expect("the generator only accepts connected boards")
            .into_iter()
            .map(|c| i64::try_from(c).expect("a cell id fits in i64"))
            .collect();
        // A straight-line walk would be `getCellDistance` steps; a real board
        // makes the A* longer than that at least sometimes, and either way the
        // path has to be the map's own.
        assert!(want.len() > 1, "the two spawns are not adjacent");
        assert_eq!(
            int_vec(&call(&mut state, "getPath", &[id(start), id(end)])),
            want,
            "getPath(spawn, spawn) must be Map::get_astar_path"
        );
        assert_value(
            &call(&mut state, "getPathLength", &[id(start), id(end)]),
            &Value::Int(i64::try_from(want.len()).expect("a path length fits in i64")),
            "getPathLength agrees with getPath",
        );
        // Same cell twice: the empty path and 0, decided *before* the A* (which
        // reports no path at all when start == end).
        assert_eq!(
            int_vec(&call(&mut state, "getPath", &[id(start), id(start)])),
            Vec::<i64>::new(),
            "getPath(c, c) is the empty array, not null"
        );
        assert_value(
            &call(&mut state, "getPathLength", &[id(start), id(start)]),
            &Value::Int(0),
            "getPathLength(c, c)",
        );
    }

    /// The caller on 306 with the row 288–306–324 otherwise clear, and one
    /// live enemy parked in the far corner (cell 0, which is not on that row).
    /// Whether `lineOfSight(288, 324)` is true then turns entirely on whether
    /// the caller's own cell is in the ignored list.
    fn a_blocked_row() -> State {
        let mut stats = Stats::default();
        stats.set(STAT_LIFE, 100);
        stats.set(STAT_TP, 10);
        stats.set(STAT_MP, 5);
        let mut state = State::new(42);
        let me = state.add_entity(0, Fighter::new(0, 1, "mine".into(), 0, stats.clone()));
        let enemy = state.add_entity(1, Fighter::new(0, 2, "theirs".into(), 1, stats));
        state.place_entity(me, 306);
        state.place_entity(enemy, 0);
        state
    }

    /// `lineOfSight`'s three ignore branches, which upstream deliberately does
    /// **not** make symmetric (`FieldClass.lineOfSight`):
    ///
    /// * no third argument ignores the caller's own cell;
    /// * a number is one entity id and ignores only *its* cell — the caller's
    ///   own cell stays blocking, which is the surprising one;
    /// * an array is a list of entity ids *plus* the caller's own cell.
    #[test]
    fn line_of_sight_ignores_the_caller_only_on_two_of_three_branches() {
        let mut state = a_blocked_row();
        assert_value(
            &call(&mut state, "lineOfSight", &[id(288), id(324)]),
            &Value::Bool(true),
            "lineOfSight(288, 324) ignores the caller standing on 306",
        );
        assert_value(
            &call(
                &mut state,
                "lineOfSight",
                &[id(288), id(324), Value::Int(1)],
            ),
            &Value::Bool(false),
            "lineOfSight(288, 324, enemy) ignores only the enemy, so 306 blocks",
        );
        let ignore_list = int_array(std::iter::once(1_i64));
        assert_value(
            &call(&mut state, "lineOfSight", &[id(288), id(324), ignore_list]),
            &Value::Bool(true),
            "lineOfSight(288, 324, [enemy]) ignores the caller as well",
        );
    }

    /// `getPath`'s ignore argument reads the *other* way round from
    /// `lineOfSight`'s: `EntityAI.putCells` resolves each array element as a
    /// **cell** id, and the caller's own cell is never added.
    #[test]
    fn get_path_ignores_cells_where_line_of_sight_ignores_entities() {
        let mut state = a_blocked_row();
        // The caller occupies 306, the one cell between 288 and 324, so the
        // plain A* has to detour.
        let detour = int_vec(&call(&mut state, "getPath", &[id(288), id(324)]));
        assert!(
            detour.len() > 2,
            "the caller on 306 must force a detour, got {detour:?}"
        );
        // Ignoring cell 306 opens the straight line: 306 then 324.
        let ignore_cells = int_array(std::iter::once(306_i64));
        assert_eq!(
            int_vec(&call(
                &mut state,
                "getPath",
                &[id(288), id(324), ignore_cells]
            )),
            vec![306, 324],
            "getPath's array is cell ids"
        );
        // The array is NOT entity ids: `[0]` names cell 0 (the enemy's corner,
        // nowhere near this row), so the detour is unchanged even though 0 is
        // also the caller's fid.
        let entity_shaped = int_array(std::iter::once(0_i64));
        assert_eq!(
            int_vec(&call(
                &mut state,
                "getPath",
                &[id(288), id(324), entity_shaped],
            )),
            detour,
            "getPath([0]) must ignore cell 0, not entity 0"
        );
        // The deprecated number form *is* an entity: fid 0 is the caller, whose
        // cell is 306, so the straight line opens again.
        assert_eq!(
            int_vec(&call(
                &mut state,
                "getPath",
                &[id(288), id(324), Value::Int(0)],
            )),
            vec![306, 324],
            "getPath(a, b, entity) is the legacy leek_to_ignore overload"
        );
    }

    // ── moveAwayFrom / moveAwayFromCell (R4-07) ──────────────────────────

    /// `one_leek` plus an enemy standing next to it, so the flee builtins
    /// have something to run away from. Fighter 0 is on 306, fighter 1 on an
    /// adjacent cell.
    fn leek_and_chaser() -> State {
        let mut state = one_leek();
        let mut stats = Stats::default();
        stats.set(STAT_LIFE, 100);
        stats.set(STAT_TP, 10);
        stats.set(STAT_MP, 5);
        let chaser = state.add_entity(1, Fighter::new(0, 2, "chaser".into(), 1, stats));
        let neighbour = state
            .map
            .cells_around(306)
            .into_iter()
            .flatten()
            .next()
            .expect("a mid-board cell has neighbours");
        state.place_entity(chaser, neighbour);
        state
    }

    /// The headline behaviour: `moveAwayFrom` puts distance between the
    /// caster and the target, spends exactly the MP it reports, and logs the
    /// move.
    #[test]
    fn move_away_from_increases_the_distance_and_charges_mp() {
        let mut state = leek_and_chaser();
        let target_cell = state.fighters[1].cell.expect("the chaser is placed");
        let before = state.map.get_distance_sq(
            state.fighters[0].cell.expect("the caster is placed"),
            target_cell,
        );
        let mp_before = state.fighters[0].mp();

        let moved = call_official_builtin(&mut state, 0, "moveAwayFrom", &[Value::Int(1)]);
        let Value::Int(steps) = moved else {
            panic!("moveAwayFrom answers an int, got {moved:?}");
        };
        assert!(
            steps > 0,
            "a leek with 5 MP on an open board can always flee"
        );

        let after = state.map.get_distance_sq(
            state.fighters[0].cell.expect("the caster is still placed"),
            target_cell,
        );
        assert!(
            after > before,
            "moveAwayFrom must strictly increase the squared distance: {before} -> {after}"
        );
        assert_eq!(
            i64::from(mp_before - state.fighters[0].mp()),
            steps,
            "the MP spent is the value returned"
        );
        assert!(
            state
                .actions
                .to_json()
                .as_array()
                .is_some_and(|a| !a.is_empty()),
            "the move is logged"
        );
    }

    /// `moveAwayFromCell` flees a cell rather than an entity — including the
    /// caster's own cell, which `moveTowardCell` refuses but this one
    /// accepts (every neighbour is strictly further from it).
    #[test]
    fn move_away_from_cell_flees_even_its_own_cell() {
        let mut state = one_leek();
        let start = state.fighters[0].cell.expect("placed");
        let start_id = i64::try_from(start).expect("a cell id fits in i64");
        let moved =
            call_official_builtin(&mut state, 0, "moveAwayFromCell", &[Value::Int(start_id)]);
        let Value::Int(steps) = moved else {
            panic!("moveAwayFromCell answers an int, got {moved:?}");
        };
        assert!(steps > 0, "fleeing your own cell is a legal request");
        assert_ne!(
            state.fighters[0].cell,
            Some(start),
            "the leek actually moved"
        );
    }

    /// Both flee builtins are behind `deny_during_hook`, like their
    /// `moveToward` twins: they answer 0 and spend nothing.
    #[test]
    fn move_away_is_denied_during_a_hook() {
        for (name, arg) in [("moveAwayFrom", 1_i64), ("moveAwayFromCell", 0)] {
            let mut state = leek_and_chaser();
            state.hook_phase = HookPhase::BeforeFight;
            let mp_before = state.fighters[0].mp();
            let cell_before = state.fighters[0].cell;
            let got = call_official_builtin(&mut state, 0, name, &[Value::Int(arg)]);
            assert_value(&got, &Value::Int(0), name);
            assert_eq!(state.fighters[0].mp(), mp_before, "{name} spent MP");
            assert_eq!(state.fighters[0].cell, cell_before, "{name} moved the leek");
        }
    }

    /// A dead target has no cell (`Map.removeEntity` nulls it), so there is
    /// nothing to flee and no MP is spent.
    #[test]
    fn move_away_from_a_cell_less_target_does_nothing() {
        let mut state = leek_and_chaser();
        state.remove_entity_from_map(1);
        let mp_before = state.fighters[0].mp();
        let got = call_official_builtin(&mut state, 0, "moveAwayFrom", &[Value::Int(1)]);
        assert_value(&got, &Value::Int(0), "moveAwayFrom(dead)");
        assert_eq!(state.fighters[0].mp(), mp_before);
    }

    // ── say (R4-07) ──────────────────────────────────────────────────────

    /// How many `[203, …]` say actions are in the report so far.
    fn say_count(state: &State) -> usize {
        state
            .actions
            .to_json()
            .as_array()
            .map(|actions| {
                actions
                    .iter()
                    .filter(|a| a.get(0) == Some(&serde_json::json!(crate::actions::SAY)))
                    .count()
            })
            .unwrap_or_default()
    }

    /// A say past `SAY_LIMIT_TURN` is dropped — but it still costs its TP,
    /// because `EntityClass.say` spends the TP *before* it tests the cap.
    #[test]
    fn a_third_say_in_one_turn_is_dropped_and_still_costs_tp() {
        let mut state = one_leek();
        let tp_before = state.fighters[0].tp();
        for i in 0..2 {
            let got = call_official_builtin(&mut state, 0, "say", &[rt_str("hello")]);
            assert_value(&got, &Value::Bool(true), &format!("say #{i}"));
        }
        assert_eq!(say_count(&state), 2, "both says are logged");

        let third = call_official_builtin(&mut state, 0, "say", &[rt_str("hello")]);
        assert_value(&third, &Value::Bool(false), "the third say of a turn");
        assert_eq!(say_count(&state), 2, "the third say logs nothing");
        assert_eq!(
            state.fighters[0].tp(),
            tp_before - 3,
            "all three says cost 1 TP, the dropped one included"
        );

        // The cap is per turn, so the counter reset lets the AI talk again.
        state.fighters[0].end_turn();
        let fourth = call_official_builtin(&mut state, 0, "say", &[rt_str("next turn")]);
        assert_value(&fourth, &Value::Bool(true), "say after end_turn");
        assert_eq!(say_count(&state), 3);
    }

    /// Out of TP, `say` answers false and logs nothing (the TP gate is
    /// tested before anything else happens).
    #[test]
    fn say_without_tp_is_refused() {
        let mut state = one_leek();
        let tp = state.fighters[0].tp();
        state.fighters[0].use_tp(tp);
        let got = call_official_builtin(&mut state, 0, "say", &[rt_str("hello")]);
        assert_value(&got, &Value::Bool(false), "say with 0 TP");
        assert_eq!(say_count(&state), 0);
        assert_eq!(
            state.fighters[0].says_turn, 0,
            "the cap counter is untouched"
        );
    }

    /// `say` is NOT a combat action: `EntityClass.say` has no
    /// `denyDuringHook`, so an AI can talk from `beforeFight()` and the
    /// message reaches the report. That difference is what makes the hook
    /// transcripts carry says at all.
    #[test]
    fn say_is_allowed_during_a_hook() {
        for phase in [HookPhase::BeforeFight, HookPhase::AfterFight] {
            let mut state = one_leek();
            state.hook_phase = phase;
            let got = call_official_builtin(&mut state, 0, "say", &[rt_str("hi from the hook")]);
            assert_value(&got, &Value::Bool(true), &format!("say during {phase:?}"));
            assert_eq!(say_count(&state), 1, "say during {phase:?} is logged");
        }
    }

    /// A long message is cut to `SAY_LENGTH_LIMIT`, and a non-string
    /// argument is stringified rather than refused.
    #[test]
    fn a_long_say_is_truncated_and_any_value_is_stringified() {
        let mut state = one_leek();
        let long = "x".repeat(250);
        let _ = call_official_builtin(&mut state, 0, "say", &[rt_str(&long)]);
        let _ = call_official_builtin(&mut state, 0, "say", &[Value::Int(42)]);
        let actions = state.actions.to_json();
        let actions = actions.as_array().expect("an action array");
        assert_eq!(actions[0][1], serde_json::json!("x".repeat(100)));
        assert_eq!(actions[1][1], serde_json::json!("42"));
    }

    // ── mark / markText / clearMarks (R4-07) ─────────────────────────────

    /// The marks are farmer-log debug output, not report actions: nothing
    /// they do reaches the action list. What an AI *can* observe is the
    /// return value, and that tracks whether the argument named a real cell.
    #[test]
    fn marks_answer_by_cell_validity_and_log_no_action() {
        let mut state = one_leek();
        let array = |ids: &[i64]| int_array(ids.iter().copied());

        for (arg, want) in [
            (Value::Int(306), true),
            (Value::Int(613), false), // one past the board
            (Value::Int(-1), false),
            (array(&[306, 307]), true),
            (array(&[900, 901]), false), // no element resolves
            (array(&[900, 306]), true),  // one does
            (array(&[]), false),
            (Value::Null, false),
            (rt_str("306"), false), // a string is not `instanceof Number`
            (Value::Bool(true), false),
        ] {
            let got = call_official_builtin(&mut state, 0, "mark", std::slice::from_ref(&arg));
            assert_value(&got, &Value::Bool(want), &format!("mark({arg:?})"));
        }

        // `markText` takes the same cell arguments...
        let got = call_official_builtin(&mut state, 0, "markText", &[Value::Int(306), rt_str("A")]);
        assert_value(&got, &Value::Bool(true), "markText(306, text)");
        let got = call_official_builtin(&mut state, 0, "markText", &[Value::Int(613), rt_str("A")]);
        assert_value(&got, &Value::Bool(false), "markText(613, text)");

        // ...and `clearMarks` is declared void, so it answers null.
        let got = call_official_builtin(&mut state, 0, "clearMarks", &[]);
        assert_value(&got, &Value::Null, "clearMarks()");

        assert_eq!(
            state.actions.to_json().as_array().map(Vec::len),
            Some(0),
            "no mark reaches the report"
        );
    }

    /// A `map<cell, text>` is `markText`'s own overload and always answers
    /// true — an empty one included, whose loop marks nothing and falls
    /// through to `return true`.
    #[test]
    fn mark_text_accepts_a_map_unconditionally() {
        let mut state = one_leek();
        let empty_map = Value::Map(std::rc::Rc::new(std::cell::RefCell::new(
            leek_runtime::MapData::default(),
        )));
        let got = call_official_builtin(&mut state, 0, "markText", &[empty_map]);
        assert_value(&got, &Value::Bool(true), "markText(empty map)");
    }

    /// A LeekScript string argument.
    fn rt_str(s: &str) -> Value {
        Value::String(std::rc::Rc::new(s.to_string()))
    }
}
