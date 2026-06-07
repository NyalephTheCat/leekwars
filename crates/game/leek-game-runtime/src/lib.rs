//! Leek-wars fight builtins — the game-side equivalent of `leek-runtime`.
//!
//! Where `leek_runtime::call_builtin` implements the standard library against
//! a [`BuiltinHost`](leek_runtime), [`call_game_builtin`] implements the
//! leek-wars *fight* functions (`getCell`, `getLife`, `moveTowardCell`,
//! `useWeapon`, …) against a [`GameHost`]. This crate holds **only the
//! functions**; the world model and fight orchestration live in the generator
//! crate, which implements `GameHost` over its state.
//!
//! Keeping the two apart means a function like `getCellDistance` is written
//! once, independent of how the generator stores the map — exactly the
//! `leek-runtime` / backend split, one layer up.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use leek_runtime::Value;

pub mod chips;
pub mod weapons;

/// A buffable combat characteristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stat {
    Strength,
    Agility,
    Wisdom,
    Resistance,
    Science,
    Magic,
    Power,
}

/// A kind of effect a weapon or chip applies. The scaling stat and whether
/// it's instant or lasts `turns` follow from the kind (see [`apply_effect`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    /// Instant damage, scaled by the caster's strength (+ crit, power,
    /// life-steal, erosion, damage-return).
    Damage,
    /// Instant heal, scaled by wisdom.
    Heal,
    /// Flat damage shield for `turns`, scaled by resistance.
    AbsoluteShield,
    /// Percent damage shield for `turns`.
    RelativeShield,
    /// A stat buff for `turns`, scaled by science. Stored with a signed value
    /// (a [`EffectKind::Shackle`] stores a negative one).
    Buff(Stat),
    /// Per-turn damage for `turns`, scaled by magic.
    Poison,
    /// Per-turn heal for `turns`, scaled by wisdom.
    Regeneration,
    /// Max-life damage (erosion), scaled by science. Instant.
    Nova,
    /// A stat debuff for `turns`, scaled by the caster's magic (stored as a
    /// negative [`EffectKind::Buff`]).
    Shackle(Stat),
    /// Negative shield for `turns` — increases damage taken. `absolute` flat
    /// vs. percent (stored as a negative shield).
    Vulnerability { absolute: bool },
    /// Raise max life and heal by the same amount, scaled by wisdom. Instant.
    Vitality,
    /// Remove all poison from the target. Instant.
    Antidote,
    /// Revive a dead target to `value` life, scaled by wisdom. Instant.
    Resurrect,
}

/// One effect of a weapon/chip: a `value1 + jet·value2` roll applied as
/// [`EffectKind`], lasting `turns` (0 = instant).
#[derive(Debug, Clone, Copy)]
pub struct Effect {
    pub kind: EffectKind,
    pub value1: i64,
    pub value2: i64,
    pub turns: i64,
}

impl Effect {
    /// Convenience constructor.
    #[must_use]
    pub const fn new(kind: EffectKind, value1: i64, value2: i64, turns: i64) -> Self {
        Self { kind, value1, value2, turns }
    }
}

/// Return codes for action functions (`useWeapon`, `useChip`), mirroring the
/// leek-wars `USE_*` constants.
pub const USE_SUCCESS: i64 = 1;
pub const USE_CRITICAL: i64 = 2;
pub const USE_FAILED: i64 = 0;
pub const USE_INVALID_TARGET: i64 = -1;
pub const USE_NOT_ENOUGH_TP: i64 = -2;
pub const USE_INVALID_POSITION: i64 = -3;
pub const USE_TOO_MANY_USES: i64 = -4;

/// Cell contents, mirroring the leek-wars `CELL_*` constants.
pub const CELL_EMPTY: i64 = 0;
pub const CELL_OBSTACLE: i64 = 1;
pub const CELL_PLAYER: i64 = 2;

/// The world-access seam the game builtins operate through. The generator
/// (which owns the fight state) implements it. Analogous to
/// `leek_runtime::BuiltinHost`, but for fight state.
///
/// Entity ids are the leek-wars integer ids; cells are 0-based grid indices.
/// Read accessors return `None` for an unknown entity/cell, which the
/// builtins surface as `null` (or a sentinel for non-optional returns).
pub trait GameHost {
    // ---- Entities (Layer: queries) ----
    /// The entity the running AI controls.
    fn current_entity(&self) -> i64;
    /// The current turn number (1-based).
    fn turn(&self) -> i64;
    /// All entity ids, or only living ones when `alive_only`.
    fn entities(&self, alive_only: bool) -> Vec<i64>;
    fn life(&self, entity: i64) -> Option<i64>;
    fn max_life(&self, entity: i64) -> Option<i64>;
    fn cell(&self, entity: i64) -> Option<i64>;
    fn team(&self, entity: i64) -> Option<i64>;
    fn name(&self, entity: i64) -> Option<String>;
    fn mp(&self, entity: i64) -> Option<i64>;
    fn tp(&self, entity: i64) -> Option<i64>;
    /// Effective strength (base + active buffs).
    fn strength(&self, entity: i64) -> Option<i64>;
    fn wisdom(&self, entity: i64) -> Option<i64>;
    fn agility(&self, entity: i64) -> Option<i64>;
    fn resistance(&self, entity: i64) -> Option<i64>;
    fn science(&self, entity: i64) -> Option<i64>;
    fn magic(&self, entity: i64) -> Option<i64>;
    fn power(&self, entity: i64) -> Option<i64>;
    fn level(&self, entity: i64) -> Option<i64>;
    fn damage_return(&self, entity: i64) -> Option<i64>;

    // ---- Map (Layer: map) ----
    /// Grid X of a cell (`None` if the cell is off-map).
    fn cell_x(&self, cell: i64) -> Option<i64>;
    /// Grid Y of a cell.
    fn cell_y(&self, cell: i64) -> Option<i64>;
    /// Cell at grid `(x, y)`, or `None` if off-map.
    fn cell_from_xy(&self, x: i64, y: i64) -> Option<i64>;
    /// The living entity standing on `cell`, if any.
    fn entity_at(&self, cell: i64) -> Option<i64>;
    /// Whether `cell` is an obstacle (blocks movement and line of sight).
    fn is_obstacle(&self, cell: i64) -> bool;
    /// All obstacle cells.
    fn obstacles(&self) -> Vec<i64>;

    // ---- Actions (Layer: actions — mutating) ----
    /// Move `entity` up to `max_mp` cells toward (or `away` from) `target`,
    /// consuming MP. Returns the number of cells actually moved.
    fn move_toward(&mut self, entity: i64, target: i64, max_mp: i64, away: bool) -> i64;
    /// Spend `amount` TP if the entity has it; returns whether it did.
    fn spend_tp(&mut self, entity: i64, amount: i64) -> bool;
    /// Apply `amount` damage to `target`'s life (clamped at 0); returns the
    /// damage actually dealt.
    fn deal_damage(&mut self, target: i64, amount: i64) -> i64;
    /// Record a chat message from `entity` (the `say` builtin).
    fn say(&mut self, entity: i64, message: &str);

    // ---- Weapons & shields (Layer: formulas) ----
    /// The entity's equipped weapon (a `WEAPON_*` item id), if any.
    fn weapon(&self, entity: i64) -> Option<i64>;
    /// The entity's weapon inventory.
    fn weapons(&self, entity: i64) -> Vec<i64>;
    /// Equip `item` if the entity owns it; returns whether it did.
    fn set_weapon(&mut self, entity: i64, item: i64) -> bool;
    /// Relative (percent) damage shield, 0 if none.
    fn relative_shield(&self, entity: i64) -> i64;
    /// Absolute (flat) damage shield, 0 if none.
    fn absolute_shield(&self, entity: i64) -> i64;
    /// A combat damage roll `jet ∈ [0, 1)` (seeded, so fights are
    /// reproducible). Advances the fight's RNG.
    fn roll_jet(&mut self) -> f64;

    /// Restore up to `amount` life to `entity` (capped at its max).
    fn heal(&mut self, entity: i64, amount: i64);
    /// Permanently reduce `entity`'s max life by `amount` (erosion), clamping
    /// current life to the new max.
    fn reduce_max_life(&mut self, entity: i64, amount: i64);
    /// Apply a lasting effect (shield / buff / poison / regeneration) to
    /// `entity` for `turns` turns. `value` is the already-scaled per-effect
    /// amount (negative for shackles / vulnerabilities).
    fn add_effect(&mut self, entity: i64, kind: EffectKind, value: i64, turns: i64);
    /// Raise `entity`'s max life by `amount` and heal it the same (vitality).
    fn grant_vitality(&mut self, entity: i64, amount: i64);
    /// Remove all active effects of `kind` from `entity` (e.g. antidote
    /// clears [`EffectKind::Poison`]).
    fn remove_effects(&mut self, entity: i64, kind: EffectKind);
    /// Revive a dead `entity` to `life` (no-op if it's alive).
    fn revive(&mut self, entity: i64, life: i64);

    /// Remaining cooldown turns for `entity`'s `item` (0 = ready).
    fn cooldown(&self, entity: i64, item: i64) -> i64;
    /// How many times `entity` used `item` this turn.
    fn uses_this_turn(&self, entity: i64, item: i64) -> i64;
    /// Record a use of `item` by `entity`, starting its `cooldown`.
    fn register_use(&mut self, entity: i64, item: i64, cooldown: i64);
}

/// Dispatch a leek-wars fight function — the game-side analogue of
/// `leek_runtime::call_builtin`. Returns [`Value::Null`] for an unknown or
/// not-yet-modeled function, so the calling AI keeps running.
#[must_use]
#[allow(clippy::too_many_lines)]
// Grid coordinates are small; the i64→f64 cast in `getDistance` can't lose
// precision in practice.
#[allow(clippy::cast_precision_loss)]
pub fn call_game_builtin(host: &mut dyn GameHost, name: &str, args: &[Value]) -> Value {
    let current = host.current_entity();
    // An entity argument, defaulting to the current entity. Captures `args`
    // and `current` only (no host borrow) so mutating arms can still take a
    // `&mut` host.
    let entity_arg = |i: usize| args.get(i).map_or(current, Value::to_long);
    let int_arg = |i: usize| args.get(i).map_or(0, Value::to_long);

    match name {
        // ---- Entity & roster queries ----
        "getEntity" => Value::Int(current),
        "getTurn" => Value::Int(host.turn()),
        "getEntities" => int_array(host.entities(false)),
        "getAliveEntities" => int_array(host.entities(true)),
        "getEnemies" => int_array(team_filter(host, current, false)),
        "getAllies" => int_array(team_filter(host, current, true)),
        "getEnemiesCount" => Value::Int(count(team_filter(host, current, false).len())),
        "getAlliesCount" => Value::Int(count(team_filter(host, current, true).len())),
        "getNearestEnemy" => opt_int(nearest(host, current, false)),
        "getNearestAlly" => opt_int(nearest(host, current, true)),
        // Visual markers — no-ops in the headless engine.
        "mark" | "markText" | "clearMarks" => Value::Bool(true),
        "getLife" => opt_int(host.life(entity_arg(0))),
        "getTotalLife" | "getMaxLife" => opt_int(host.max_life(entity_arg(0))),
        "getCell" => opt_int(host.cell(entity_arg(0))),
        "getTeam" => opt_int(host.team(entity_arg(0))),
        "getName" => host.name(entity_arg(0)).map_or(Value::Null, string_val),
        "getMP" => opt_int(host.mp(entity_arg(0))),
        "getTP" => opt_int(host.tp(entity_arg(0))),
        "getStrength" => opt_int(host.strength(entity_arg(0))),
        "getWisdom" => opt_int(host.wisdom(entity_arg(0))),
        "getAgility" => opt_int(host.agility(entity_arg(0))),
        "getResistance" => opt_int(host.resistance(entity_arg(0))),
        "getScience" => opt_int(host.science(entity_arg(0))),
        "getMagic" => opt_int(host.magic(entity_arg(0))),
        "getPower" => opt_int(host.power(entity_arg(0))),
        "getLevel" => opt_int(host.level(entity_arg(0))),
        "getDamageReturn" => opt_int(host.damage_return(entity_arg(0))),
        "getAbsoluteShield" => opt_int(Some(host.absolute_shield(entity_arg(0)))),
        "getRelativeShield" => opt_int(Some(host.relative_shield(entity_arg(0)))),
        "getWeapon" => opt_int(host.weapon(entity_arg(0))),
        "getWeapons" => int_array(host.weapons(entity_arg(0))),
        "setWeapon" => Value::Bool(host.set_weapon(current, int_arg(0))),
        // getCooldown(item, [entity]).
        "getCooldown" => Value::Int(host.cooldown(entity_arg(1), int_arg(0))),
        "isAlive" => Value::Bool(host.life(entity_arg(0)).is_some_and(|l| l > 0)),
        "isDead" => Value::Bool(host.life(entity_arg(0)).is_none_or(|l| l <= 0)),

        // ---- Map / geometry ----
        "getCellX" => opt_int(host.cell_x(int_arg(0))),
        "getCellY" => opt_int(host.cell_y(int_arg(0))),
        "getCellFromXY" => opt_int(host.cell_from_xy(int_arg(0), int_arg(1))),
        "getCellContent" => {
            let cell = int_arg(0);
            if host.is_obstacle(cell) {
                Value::Int(CELL_OBSTACLE)
            } else if host.entity_at(cell).is_some() {
                Value::Int(CELL_PLAYER)
            } else {
                Value::Int(CELL_EMPTY)
            }
        }
        "getObstacles" => int_array(host.obstacles()),
        "lineOfSight" => Value::Bool(line_of_sight(host, int_arg(0), int_arg(1))),
        "getPath" => match bfs_path(host, int_arg(0), int_arg(1)) {
            Some(path) => int_array(path),
            None => Value::Null,
        },
        "getPathLength" => match bfs_path(host, int_arg(0), int_arg(1)) {
            Some(path) => Value::Int(i64::try_from(path.len()).unwrap_or(-1)),
            None => Value::Int(-1),
        },
        "getCellDistance" => match coords(host, int_arg(0), int_arg(1)) {
            Some((x1, y1, x2, y2)) => Value::Int((x1 - x2).abs() + (y1 - y2).abs()),
            None => Value::Int(-1),
        },
        "getDistance" => match coords(host, int_arg(0), int_arg(1)) {
            Some((x1, y1, x2, y2)) => {
                let (dx, dy) = ((x1 - x2) as f64, (y1 - y2) as f64);
                Value::Real(dx.hypot(dy))
            }
            None => Value::Real(-1.0),
        },
        "isOnSameLine" => match coords(host, int_arg(0), int_arg(1)) {
            Some((x1, y1, x2, y2)) => {
                Value::Bool(x1 == x2 || y1 == y2 || (x1 - x2).abs() == (y1 - y2).abs())
            }
            None => Value::Bool(false),
        },

        // ---- Movement (mutating) ----
        "moveTowardCell" => Value::Int(host.move_toward(current, int_arg(0), mp_arg(args, 1), false)),
        "moveAwayFromCell" => {
            Value::Int(host.move_toward(current, int_arg(0), mp_arg(args, 1), true))
        }
        "moveToward" => move_to_entity(host, current, entity_arg(0), mp_arg(args, 1), false),
        "moveAwayFrom" => move_to_entity(host, current, entity_arg(0), mp_arg(args, 1), true),

        // ---- Combat (mutating) ----
        // `useWeapon` applies the real leek-wars damage formula against the
        // equipped weapon's catalog stats (range, TP cost, strength scaling,
        // shields). `useChip` is still a flat placeholder pending a chip
        // catalog.
        "useWeapon" => use_weapon(host, current, int_arg(0)),
        "useChip" => use_chip(host, current, int_arg(0), int_arg(1)),

        // ---- Communication ----
        "say" => {
            host.say(current, &message_text(args.first()));
            Value::Bool(true)
        }

        _ => Value::Null,
    }
}

/// Whether `name` is a fight function this runtime dispatches.
#[must_use]
pub fn is_game_builtin(name: &str) -> bool {
    matches!(
        name,
        "getEntity"
            | "getTurn"
            | "getEntities"
            | "getAliveEntities"
            | "getEnemies"
            | "getAllies"
            | "getEnemiesCount"
            | "getAlliesCount"
            | "getNearestEnemy"
            | "getNearestAlly"
            | "mark"
            | "markText"
            | "clearMarks"
            | "getLife"
            | "getTotalLife"
            | "getMaxLife"
            | "getCell"
            | "getTeam"
            | "getName"
            | "getMP"
            | "getTP"
            | "getStrength"
            | "getWisdom"
            | "getAgility"
            | "getResistance"
            | "getScience"
            | "getMagic"
            | "getPower"
            | "getLevel"
            | "getDamageReturn"
            | "getAbsoluteShield"
            | "getRelativeShield"
            | "getWeapon"
            | "getWeapons"
            | "setWeapon"
            | "getCooldown"
            | "isAlive"
            | "isDead"
            | "getCellX"
            | "getCellY"
            | "getCellFromXY"
            | "getCellContent"
            | "getObstacles"
            | "lineOfSight"
            | "getPath"
            | "getPathLength"
            | "getCellDistance"
            | "getDistance"
            | "isOnSameLine"
            | "moveTowardCell"
            | "moveAwayFromCell"
            | "moveToward"
            | "moveAwayFrom"
            | "useWeapon"
            | "useChip"
            | "say"
    )
}

/// Shortest grid path from `start` to `end` (4-neighbour BFS), avoiding
/// obstacles and occupied cells. Returns the cells to traverse after `start`
/// (ending at `end`), or `None` if unreachable. Empty when already there.
fn bfs_path(host: &dyn GameHost, start: i64, end: i64) -> Option<Vec<i64>> {
    if start == end {
        return Some(Vec::new());
    }
    let mut visited: HashSet<i64> = HashSet::from([start]);
    let mut prev: HashMap<i64, i64> = HashMap::new();
    let mut queue: VecDeque<i64> = VecDeque::from([start]);
    while let Some(cell) = queue.pop_front() {
        let (Some(x), Some(y)) = (host.cell_x(cell), host.cell_y(cell)) else {
            continue;
        };
        for (nx, ny) in [(x + 1, y), (x - 1, y), (x, y + 1), (x, y - 1)] {
            let Some(n) = host.cell_from_xy(nx, ny) else {
                continue;
            };
            if visited.contains(&n) || host.is_obstacle(n) || host.entity_at(n).is_some() {
                continue;
            }
            visited.insert(n);
            prev.insert(n, cell);
            if n == end {
                // Reconstruct start→end, dropping `start`.
                let mut path = vec![end];
                let mut c = end;
                while let Some(&p) = prev.get(&c) {
                    if p == start {
                        break;
                    }
                    path.push(p);
                    c = p;
                }
                path.reverse();
                return Some(path);
            }
            queue.push_back(n);
        }
    }
    None
}

/// Whether there's a clear line of sight from `c1` to `c2`: no obstacle on
/// any cell strictly between them (Bresenham over the grid).
fn line_of_sight(host: &dyn GameHost, c1: i64, c2: i64) -> bool {
    let (Some(mut x0), Some(mut y0), Some(x1), Some(y1)) =
        (host.cell_x(c1), host.cell_y(c1), host.cell_x(c2), host.cell_y(c2))
    else {
        return false;
    };
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;
    let mut first = true;
    loop {
        let at_target = x0 == x1 && y0 == y1;
        if !first
            && !at_target
            && let Some(cell) = host.cell_from_xy(x0, y0)
            && host.is_obstacle(cell)
        {
            return false;
        }
        if at_target {
            return true;
        }
        first = false;
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            y0 += sy;
        }
    }
}

/// `(x1, y1, x2, y2)` of two cells, or `None` if either is off-map.
fn coords(host: &dyn GameHost, c1: i64, c2: i64) -> Option<(i64, i64, i64, i64)> {
    Some((
        host.cell_x(c1)?,
        host.cell_y(c1)?,
        host.cell_x(c2)?,
        host.cell_y(c2)?,
    ))
}

/// Move toward another entity's cell (helper for `moveToward`/`moveAwayFrom`).
fn move_to_entity(host: &mut dyn GameHost, mover: i64, target: i64, max_mp: i64, away: bool) -> Value {
    match host.cell(target) {
        Some(cell) => Value::Int(host.move_toward(mover, cell, max_mp, away)),
        None => Value::Int(0),
    }
}

/// `useWeapon(target)`: fire the attacker's equipped weapon. Checks weapon,
/// range, and TP, then applies its effects.
fn use_weapon(host: &mut dyn GameHost, attacker: i64, target: i64) -> Value {
    let Some(item) = host.weapon(attacker) else {
        return Value::Int(USE_FAILED); // no weapon equipped
    };
    let Some(weapon) = weapons::lookup(item) else {
        return Value::Int(USE_FAILED); // weapon not modeled
    };
    let effects: Vec<Effect> = weapon
        .damages
        .iter()
        .map(|&(v1, v2)| Effect::new(EffectKind::Damage, v1, v2, 0))
        .collect();
    use_effects(host, attacker, target, weapon, &effects)
}

/// `useChip(chip, target)`: cast a chip from the catalog onto the target.
fn use_chip(host: &mut dyn GameHost, caster: i64, chip_item: i64, target: i64) -> Value {
    let Some(chip) = chips::lookup(chip_item) else {
        return Value::Int(USE_FAILED); // chip not modeled
    };
    use_effects(host, caster, target, chip, chip.effects)
}

/// The use-rule stats shared by weapons and chips.
struct UseSpec {
    item: i64,
    cost: i64,
    min_range: i64,
    max_range: i64,
    area: i64,
    cooldown: i64,
    max_uses: i64,
}

impl From<&weapons::Weapon> for UseSpec {
    fn from(w: &weapons::Weapon) -> Self {
        Self { item: w.item, cost: w.cost, min_range: w.min_range, max_range: w.max_range, area: w.area, cooldown: w.cooldown, max_uses: w.max_uses }
    }
}
impl From<&chips::Chip> for UseSpec {
    fn from(c: &chips::Chip) -> Self {
        Self { item: c.item, cost: c.cost, min_range: c.min_range, max_range: c.max_range, area: c.area, cooldown: c.cooldown, max_uses: c.max_uses }
    }
}

/// Shared weapon/chip use: range + line-of-sight + TP checks, then apply each
/// effect to every entity in the area centered on the target.
#[allow(clippy::too_many_arguments)]
fn use_effects(
    host: &mut dyn GameHost,
    caster: i64,
    target: i64,
    spec: impl Into<UseSpec>,
    effects: &[Effect],
) -> Value {
    let spec = spec.into();
    let (Some(cc), Some(tc)) = (host.cell(caster), host.cell(target)) else {
        return Value::Int(USE_INVALID_TARGET);
    };
    match coords(host, cc, tc) {
        Some((x1, y1, x2, y2)) => {
            let dist = (x1 - x2).abs() + (y1 - y2).abs();
            if dist < spec.min_range || dist > spec.max_range {
                return Value::Int(USE_INVALID_TARGET);
            }
        }
        None => return Value::Int(USE_INVALID_TARGET),
    }
    if !line_of_sight(host, cc, tc) {
        return Value::Int(USE_INVALID_POSITION);
    }
    // Cooldown / per-turn use limits.
    if host.cooldown(caster, spec.item) > 0
        || (spec.max_uses > 0 && host.uses_this_turn(caster, spec.item) >= spec.max_uses)
    {
        return Value::Int(USE_TOO_MANY_USES);
    }
    if !host.spend_tp(caster, spec.cost) {
        return Value::Int(USE_NOT_ENOUGH_TP);
    }
    host.register_use(caster, spec.item, spec.cooldown);

    // One critical roll per use (chance = caster agility / 1000), applied to
    // every effect and every entity in the area.
    let critical = host.roll_jet() < f64::from(i32::try_from(stat(host.agility(caster))).unwrap_or(0)) / 1000.0;

    // Affected = the explicit target (always, so e.g. resurrection can hit a
    // dead entity) plus any living entity within the area radius of `tc`.
    let radius = (spec.area - 1).max(0);
    let mut affected = vec![target];
    if radius > 0 {
        for e in host.entities(true) {
            if e != target && manhattan(host, host.cell(e).unwrap_or(-1), tc) <= radius {
                affected.push(e);
            }
        }
    }
    for entity in affected {
        for effect in effects {
            apply_effect(host, caster, entity, effect, critical);
        }
    }
    Value::Int(if critical { USE_CRITICAL } else { USE_SUCCESS })
}

/// Living entities on the same team as `current` (`allies`) or a different
/// team (enemies), excluding `current` itself.
fn team_filter(host: &dyn GameHost, current: i64, allies: bool) -> Vec<i64> {
    let my_team = host.team(current);
    host.entities(true)
        .into_iter()
        .filter(|&e| e != current && (host.team(e) == my_team) == allies)
        .collect()
}

/// The nearest ally/enemy to `current` by grid distance, if any.
fn nearest(host: &dyn GameHost, current: i64, allies: bool) -> Option<i64> {
    let cc = host.cell(current)?;
    team_filter(host, current, allies)
        .into_iter()
        .min_by_key(|&e| host.cell(e).map_or(i64::MAX, |ec| manhattan(host, cc, ec)))
}

/// Grid (Manhattan) distance between two cells, or `i64::MAX` if either is
/// off-map.
fn manhattan(host: &dyn GameHost, c1: i64, c2: i64) -> i64 {
    coords(host, c1, c2).map_or(i64::MAX, |(x1, y1, x2, y2)| (x1 - x2).abs() + (y1 - y2).abs())
}

/// `i64` stat as `f64`, treating `None`/negatives as 0.
fn stat(s: Option<i64>) -> i64 {
    s.unwrap_or(0)
}

/// A collection length as an `i64`.
fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(0)
}

const CRITICAL_FACTOR: f64 = 1.3;
const EROSION_DAMAGE: f64 = 0.05;
const EROSION_CRITICAL_BONUS: f64 = 0.10;

/// Apply one effect from `caster` to `target`, following the leek-wars
/// formulas: each kind rolls `value1 + jet·value2`, scaled by the relevant
/// caster stat and the critical multiplier. Instant kinds (damage / heal) act
/// now; lasting kinds (shields / buffs / poison) last `turns` turns.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn apply_effect(host: &mut dyn GameHost, caster: i64, target: i64, effect: &Effect, critical: bool) {
    let jet = host.roll_jet();
    let base = effect.value1 as f64 + jet * effect.value2 as f64;
    let crit_power = if critical { CRITICAL_FACTOR } else { 1.0 };
    let scale = |s: i64| 1.0 + f64::from(i32::try_from(s.max(0)).unwrap_or(i32::MAX)) / 100.0;
    let as_f = |v: i64| f64::from(i32::try_from(v).unwrap_or(0));
    let rounded = |v: f64| v.max(0.0).round() as i64;

    match effect.kind {
        EffectKind::Damage => {
            let power = scale(stat(host.power(caster)));
            let mut d = base * scale(stat(host.strength(caster))) * crit_power * power;
            // Damage return (computed pre-shield, reflected to the caster).
            let return_dmg = if caster == target {
                0
            } else {
                rounded(d * as_f(stat(host.damage_return(target))) / 100.0)
            };
            // Shields.
            d -= d * (as_f(host.relative_shield(target)) / 100.0) + as_f(host.absolute_shield(target));
            let dealt = host.deal_damage(target, rounded(d));
            // Erosion: a fraction of the damage permanently cuts max life.
            let erosion_rate = EROSION_DAMAGE + if critical { EROSION_CRITICAL_BONUS } else { 0.0 };
            host.reduce_max_life(target, rounded(as_f(dealt) * erosion_rate));
            // Life steal: caster heals for damage × wisdom / 1000.
            if caster != target {
                let steal = rounded(as_f(dealt) * as_f(stat(host.wisdom(caster))) / 1000.0);
                host.heal(caster, steal);
            }
            // Return damage hits the caster (with its own erosion).
            if return_dmg > 0 {
                let dealt_back = host.deal_damage(caster, return_dmg);
                host.reduce_max_life(caster, rounded(as_f(dealt_back) * erosion_rate));
            }
        }
        EffectKind::Heal => {
            let v = rounded(base * scale(stat(host.wisdom(caster))) * crit_power);
            host.heal(target, v);
        }
        EffectKind::AbsoluteShield => {
            let v = rounded(base * scale(stat(host.resistance(caster))) * crit_power);
            host.add_effect(target, EffectKind::AbsoluteShield, v, effect.turns);
        }
        EffectKind::RelativeShield => {
            let v = rounded(base * scale(stat(host.resistance(caster))) * crit_power);
            host.add_effect(target, EffectKind::RelativeShield, v, effect.turns);
        }
        EffectKind::Buff(s) => {
            let v = rounded(base * scale(stat(host.science(caster))) * crit_power);
            host.add_effect(target, EffectKind::Buff(s), v, effect.turns);
        }
        EffectKind::Poison => {
            let v = rounded(base * scale(stat(host.magic(caster))) * crit_power);
            host.add_effect(target, EffectKind::Poison, v, effect.turns);
        }
        EffectKind::Regeneration => {
            let v = rounded(base * scale(stat(host.wisdom(caster))) * crit_power);
            host.add_effect(target, EffectKind::Regeneration, v, effect.turns);
        }
        EffectKind::Nova => {
            // Science-scaled max-life damage, capped so it can't drop max
            // below current life.
            let d = base * scale(stat(host.science(caster))) * crit_power
                * scale(stat(host.power(caster)));
            let headroom = (stat(host.max_life(target)) - stat(host.life(target))).max(0);
            host.reduce_max_life(target, rounded(d).min(headroom));
        }
        EffectKind::Shackle(s) => {
            // Magic-scaled stat debuff: a negative buff.
            let v = rounded(base * scale(stat(host.magic(caster))) * crit_power);
            host.add_effect(target, EffectKind::Buff(s), -v, effect.turns);
        }
        EffectKind::Vulnerability { absolute } => {
            // A negative shield (increases damage taken). Not stat-scaled.
            let v = rounded(base * crit_power);
            let kind = if absolute {
                EffectKind::AbsoluteShield
            } else {
                EffectKind::RelativeShield
            };
            host.add_effect(target, kind, -v, effect.turns);
        }
        EffectKind::Vitality => {
            let v = rounded(base * scale(stat(host.wisdom(caster))) * crit_power);
            host.grant_vitality(target, v);
        }
        EffectKind::Antidote => {
            host.remove_effects(target, EffectKind::Poison);
        }
        EffectKind::Resurrect => {
            let v = rounded(base * scale(stat(host.wisdom(caster))) * crit_power);
            host.revive(target, v);
        }
    }
}

/// The raw text of a `say` argument — a string's content (unquoted), or the
/// display form of any other value.
fn message_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => (**s).clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// MP argument `i`, defaulting to "all remaining MP" (`i64::MAX`) when absent.
fn mp_arg(args: &[Value], i: usize) -> i64 {
    args.get(i).map_or(i64::MAX, Value::to_long)
}

fn opt_int(v: Option<i64>) -> Value {
    v.map_or(Value::Null, Value::Int)
}

fn string_val(s: String) -> Value {
    Value::String(Rc::new(s))
}

fn int_array(ids: Vec<i64>) -> Value {
    Value::Array(Rc::new(RefCell::new(ids.into_iter().map(Value::Int).collect())))
}
