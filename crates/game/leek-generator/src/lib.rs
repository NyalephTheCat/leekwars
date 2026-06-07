//! Leek-wars fight orchestrator — the `leek-wars-generator` equivalent.
//!
//! Holds the **world model** (the [`Fight`]: entities on a grid map) and
//! **launches AIs**: it installs itself as the native backend's game runtime,
//! runs each entity's compiled script, and routes the fight builtins those
//! scripts call back to its own state via
//! [`leek_game_runtime::call_game_builtin`].
//!
//! Layering (state + orchestration here, functions in [`leek_game_runtime`],
//! execution in [`leek_backend_native`]) joins at the [`GameHost`] (state
//! access) and [`GameRuntime`](leek_backend_native::GameRuntime) (execution)
//! seams.
//!
//! # Layers
//!
//! - **Map**: a square grid (`width × height`), cells numbered row-major.
//!   Geometry uses grid (Manhattan) distance for movement and Euclidean for
//!   `getDistance`. The real leek-wars diamond grid can be substituted by
//!   changing the coordinate methods only.
//! - **Actions**: movement consumes MP and walks the grid greedily; combat
//!   spends TP and applies strength-based damage (a placeholder formula
//!   pending a weapon/chip catalog).
//! - **Turn loop**: [`run_fight`] runs each living entity's AI once per turn,
//!   regenerating MP/TP, until one team remains or `max_turns` elapses.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use leek_backend_native::{NativeError, NativeOptions};
use leek_game_runtime::{call_game_builtin, EffectKind, GameHost, Stat};
use leek_hir::HirFile;
use leek_runtime::Value;

/// A lasting effect on an entity (shield / buff / poison) with a remaining
/// duration in turns.
#[derive(Debug, Clone, Copy)]
pub struct ActiveEffect {
    pub kind: EffectKind,
    pub value: i64,
    pub turns: i64,
}

/// One combatant in the fight.
#[derive(Debug, Clone)]
pub struct Entity {
    pub id: i64,
    pub name: String,
    pub life: i64,
    pub max_life: i64,
    /// The cell the entity stands on.
    pub cell: i64,
    pub team: i64,
    pub mp: i64,
    pub max_mp: i64,
    pub tp: i64,
    pub max_tp: i64,
    /// Base stats (effective values add active buffs — see [`Fight`]'s
    /// `GameHost` impl).
    pub strength: i64,
    pub wisdom: i64,
    pub agility: i64,
    pub resistance: i64,
    pub science: i64,
    pub magic: i64,
    pub power: i64,
    pub level: i64,
    pub damage_return: i64,
    /// Equipped weapon (a `WEAPON_*` item id), if any.
    pub weapon: Option<i64>,
    /// Owned weapons (`WEAPON_*` item ids).
    pub inventory: Vec<i64>,
    /// Base relative (percent) damage shield.
    pub relative_shield: i64,
    /// Base absolute (flat) damage shield.
    pub absolute_shield: i64,
    /// Active lasting effects (shields / buffs / poison).
    pub effects: Vec<ActiveEffect>,
    /// Remaining cooldown per item id.
    pub item_cooldowns: HashMap<i64, i64>,
    /// Uses this turn per item id (reset each turn).
    pub item_uses: HashMap<i64, i64>,
}

impl Entity {
    /// An entity with default stats (100 life, 5 MP, 10 TP, 0 strength) on
    /// `cell`. Tune with the `with_*` builders.
    #[must_use]
    pub fn new(id: i64, name: impl Into<String>, cell: i64, team: i64) -> Self {
        Self {
            id,
            name: name.into(),
            life: 100,
            max_life: 100,
            cell,
            team,
            mp: 5,
            max_mp: 5,
            tp: 10,
            max_tp: 10,
            strength: 0,
            wisdom: 0,
            agility: 0,
            resistance: 0,
            science: 0,
            magic: 0,
            power: 0,
            level: 1,
            damage_return: 0,
            weapon: None,
            inventory: Vec::new(),
            relative_shield: 0,
            absolute_shield: 0,
            effects: Vec::new(),
            item_cooldowns: HashMap::new(),
            item_uses: HashMap::new(),
        }
    }

    /// Set the magic stats used by effect formulas (wisdom→heal,
    /// resistance→shield, science→buff, magic→poison).
    #[must_use]
    pub fn with_magic_stats(mut self, wisdom: i64, resistance: i64, science: i64, magic: i64) -> Self {
        self.wisdom = wisdom;
        self.resistance = resistance;
        self.science = science;
        self.magic = magic;
        self
    }

    /// Add `item` to the inventory and equip it.
    #[must_use]
    pub fn with_weapon(mut self, item: i64) -> Self {
        if !self.inventory.contains(&item) {
            self.inventory.push(item);
        }
        self.weapon = Some(item);
        self
    }

    /// Set relative (percent) and absolute (flat) damage shields.
    #[must_use]
    pub fn with_shields(mut self, relative: i64, absolute: i64) -> Self {
        self.relative_shield = relative;
        self.absolute_shield = absolute;
        self
    }

    /// Sum the values of active effects of `kind` (for effective stats).
    fn effect_sum(&self, kind: EffectKind) -> i64 {
        self.effects.iter().filter(|e| e.kind == kind).map(|e| e.value).sum()
    }

    /// Sum active buffs to `stat` (added to the base value by the getters).
    fn buff_sum(&self, stat: Stat) -> i64 {
        self.effect_sum(EffectKind::Buff(stat))
    }

    #[must_use]
    pub fn with_life(mut self, life: i64) -> Self {
        self.life = life;
        self.max_life = life;
        self
    }

    #[must_use]
    pub fn with_strength(mut self, strength: i64) -> Self {
        self.strength = strength;
        self
    }

    #[must_use]
    pub fn with_points(mut self, mp: i64, tp: i64) -> Self {
        self.mp = mp;
        self.max_mp = mp;
        self.tp = tp;
        self.max_tp = tp;
        self
    }
}

/// The fight world model — entities on a `width × height` grid.
#[derive(Debug, Clone)]
pub struct Fight {
    entities: Vec<Entity>,
    /// The entity whose AI is currently running (subject of no-arg queries).
    current: i64,
    width: i64,
    height: i64,
    /// Current turn number (1-based; 0 before the fight starts).
    turn: i64,
    /// Cells that block movement and line of sight.
    obstacles: HashSet<i64>,
    /// `say` output, as `(entity, message)`.
    log: Vec<(i64, String)>,
    /// Combat RNG state (xorshift64) — seeded so fights are reproducible.
    rng: u64,
}

impl Fight {
    /// A fight on a `width × height` grid with the given current entity id.
    #[must_use]
    pub fn new(width: i64, height: i64, current: i64) -> Self {
        Self {
            entities: Vec::new(),
            current,
            width,
            height,
            turn: 0,
            obstacles: HashSet::new(),
            log: Vec::new(),
            rng: 0x2545_f491_4f6c_dd1d, // default non-zero seed
        }
    }

    /// Mark `cell` as an obstacle (blocks movement and line of sight).
    #[must_use]
    pub fn with_obstacle(mut self, cell: i64) -> Self {
        self.obstacles.insert(cell);
        self
    }

    /// Seed the combat RNG (damage rolls) for a reproducible fight.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.rng = if seed == 0 { 1 } else { seed };
        self
    }

    #[must_use]
    pub fn with_entity(mut self, entity: Entity) -> Self {
        self.entities.push(entity);
        self
    }

    /// Set which entity's AI is running.
    pub fn set_current(&mut self, entity: i64) {
        self.current = entity;
    }

    /// Reset an entity's MP/TP to their maxima (start-of-turn regen).
    pub fn regen(&mut self, entity: i64) {
        if let Some(e) = self.entity_mut(entity) {
            e.mp = e.max_mp;
            e.tp = e.max_tp;
            // Reset per-turn use counts; tick item cooldowns down.
            e.item_uses.clear();
            e.item_cooldowns.retain(|_, cd| {
                *cd -= 1;
                *cd > 0
            });
        }
    }

    /// Start-of-turn effect tick for `entity`: apply poison damage, then
    /// decrement every active effect's duration and drop the expired ones.
    pub fn tick_effects(&mut self, entity: i64) {
        let Some(e) = self.entity_mut(entity) else {
            return;
        };
        let mut poison = 0;
        let mut regen = 0;
        e.effects.retain_mut(|eff| {
            match eff.kind {
                EffectKind::Poison => poison += eff.value,
                EffectKind::Regeneration => regen += eff.value,
                _ => {}
            }
            eff.turns -= 1;
            eff.turns > 0
        });
        if poison > 0 {
            // Poison damages life and erodes max life (EROSION_POISON = 0.10).
            #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
            let erosion = (f64::from(i32::try_from(poison).unwrap_or(0)) * 0.10).round() as i64;
            e.life = (e.life - poison).max(0);
            e.max_life = (e.max_life - erosion).max(0);
        }
        if regen > 0 {
            e.life += regen;
        }
        e.life = e.life.min(e.max_life);
    }

    /// The `say` messages emitted so far.
    #[must_use]
    pub fn log(&self) -> &[(i64, String)] {
        &self.log
    }

    /// The teams with at least one living entity.
    #[must_use]
    pub fn living_teams(&self) -> Vec<i64> {
        let mut teams: Vec<i64> = self
            .entities
            .iter()
            .filter(|e| e.life > 0)
            .map(|e| e.team)
            .collect();
        teams.sort_unstable();
        teams.dedup();
        teams
    }

    fn get(&self, id: i64) -> Option<&Entity> {
        self.entities.iter().find(|e| e.id == id)
    }
    fn entity_mut(&mut self, id: i64) -> Option<&mut Entity> {
        self.entities.iter_mut().find(|e| e.id == id)
    }
    fn in_bounds(&self, x: i64, y: i64) -> bool {
        x >= 0 && x < self.width && y >= 0 && y < self.height
    }
}

impl GameHost for Fight {
    fn current_entity(&self) -> i64 {
        self.current
    }
    fn turn(&self) -> i64 {
        self.turn
    }
    fn entities(&self, alive_only: bool) -> Vec<i64> {
        self.entities
            .iter()
            .filter(|e| !alive_only || e.life > 0)
            .map(|e| e.id)
            .collect()
    }
    fn life(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.life)
    }
    fn max_life(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.max_life)
    }
    fn cell(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.cell)
    }
    fn team(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.team)
    }
    fn name(&self, entity: i64) -> Option<String> {
        self.get(entity).map(|e| e.name.clone())
    }
    fn mp(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.mp)
    }
    fn tp(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.tp)
    }
    fn strength(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.strength + e.buff_sum(Stat::Strength))
    }
    fn wisdom(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.wisdom + e.buff_sum(Stat::Wisdom))
    }
    fn agility(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.agility + e.buff_sum(Stat::Agility))
    }
    fn resistance(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.resistance + e.buff_sum(Stat::Resistance))
    }
    fn science(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.science + e.buff_sum(Stat::Science))
    }
    fn magic(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.magic + e.buff_sum(Stat::Magic))
    }
    fn power(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.power + e.buff_sum(Stat::Power))
    }
    fn level(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.level)
    }
    fn damage_return(&self, entity: i64) -> Option<i64> {
        self.get(entity).map(|e| e.damage_return)
    }

    fn cell_x(&self, cell: i64) -> Option<i64> {
        (cell >= 0 && cell < self.width * self.height).then_some(cell % self.width)
    }
    fn cell_y(&self, cell: i64) -> Option<i64> {
        (cell >= 0 && cell < self.width * self.height).then_some(cell / self.width)
    }
    fn cell_from_xy(&self, x: i64, y: i64) -> Option<i64> {
        self.in_bounds(x, y).then_some(y * self.width + x)
    }
    fn entity_at(&self, cell: i64) -> Option<i64> {
        self.entities
            .iter()
            .find(|e| e.life > 0 && e.cell == cell)
            .map(|e| e.id)
    }
    fn is_obstacle(&self, cell: i64) -> bool {
        self.obstacles.contains(&cell)
    }
    fn obstacles(&self) -> Vec<i64> {
        let mut cells: Vec<i64> = self.obstacles.iter().copied().collect();
        cells.sort_unstable();
        cells
    }

    fn move_toward(&mut self, entity: i64, target: i64, max_mp: i64, away: bool) -> i64 {
        let width = self.width;
        let (Some(tx), Some(ty)) = (self.cell_x(target), self.cell_y(target)) else {
            return 0;
        };
        let Some(idx) = self.entities.iter().position(|e| e.id == entity) else {
            return 0;
        };
        let budget = self.entities[idx].mp.min(max_mp).max(0);
        let mut moved = 0;
        for _ in 0..budget {
            let cell = self.entities[idx].cell;
            let (cx, cy) = (cell % width, cell / width);
            let (dx, dy) = (tx - cx, ty - cy);
            if !away && dx == 0 && dy == 0 {
                break; // reached the target cell
            }
            // Step one cell along the dominant axis, toward or away.
            let dir = if away { -1 } else { 1 };
            let (mut nx, mut ny) = (cx, cy);
            if dx.abs() >= dy.abs() && dx != 0 {
                nx += dx.signum() * dir;
            } else if dy != 0 {
                ny += dy.signum() * dir;
            } else if away {
                nx += 1; // on the target, moving away: any in-bounds step
            } else {
                break;
            }
            if !self.in_bounds(nx, ny) {
                break;
            }
            let ncell = ny * width + nx;
            // Don't walk onto an obstacle or an occupied cell.
            if self.obstacles.contains(&ncell)
                || self
                    .entities
                    .iter()
                    .any(|o| o.id != entity && o.life > 0 && o.cell == ncell)
            {
                break;
            }
            self.entities[idx].cell = ncell;
            self.entities[idx].mp -= 1;
            moved += 1;
        }
        moved
    }

    fn spend_tp(&mut self, entity: i64, amount: i64) -> bool {
        match self.entity_mut(entity) {
            Some(e) if e.tp >= amount => {
                e.tp -= amount;
                true
            }
            _ => false,
        }
    }

    fn deal_damage(&mut self, target: i64, amount: i64) -> i64 {
        match self.entity_mut(target) {
            Some(e) => {
                let dealt = amount.min(e.life).max(0);
                e.life -= dealt;
                dealt
            }
            None => 0,
        }
    }

    fn say(&mut self, entity: i64, message: &str) {
        self.log.push((entity, message.to_string()));
    }

    fn weapon(&self, entity: i64) -> Option<i64> {
        self.get(entity).and_then(|e| e.weapon)
    }
    fn weapons(&self, entity: i64) -> Vec<i64> {
        self.get(entity).map(|e| e.inventory.clone()).unwrap_or_default()
    }
    fn set_weapon(&mut self, entity: i64, item: i64) -> bool {
        match self.entity_mut(entity) {
            Some(e) if e.inventory.contains(&item) => {
                e.weapon = Some(item);
                true
            }
            _ => false,
        }
    }
    fn relative_shield(&self, entity: i64) -> i64 {
        self.get(entity)
            .map_or(0, |e| e.relative_shield + e.effect_sum(EffectKind::RelativeShield))
    }
    fn absolute_shield(&self, entity: i64) -> i64 {
        self.get(entity)
            .map_or(0, |e| e.absolute_shield + e.effect_sum(EffectKind::AbsoluteShield))
    }
    #[allow(clippy::cast_precision_loss)]
    fn roll_jet(&mut self) -> f64 {
        // xorshift64, then the top 53 bits to a double in [0, 1).
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    fn heal(&mut self, entity: i64, amount: i64) {
        if let Some(e) = self.entity_mut(entity) {
            e.life = (e.life + amount.max(0)).min(e.max_life);
        }
    }

    fn reduce_max_life(&mut self, entity: i64, amount: i64) {
        if let Some(e) = self.entity_mut(entity) {
            e.max_life = (e.max_life - amount.max(0)).max(0);
            e.life = e.life.min(e.max_life);
        }
    }

    fn add_effect(&mut self, entity: i64, kind: EffectKind, value: i64, turns: i64) {
        if let Some(e) = self.entity_mut(entity) {
            e.effects.push(ActiveEffect { kind, value, turns });
        }
    }

    fn grant_vitality(&mut self, entity: i64, amount: i64) {
        if let Some(e) = self.entity_mut(entity) {
            let amount = amount.max(0);
            e.max_life += amount;
            e.life += amount;
        }
    }

    fn remove_effects(&mut self, entity: i64, kind: EffectKind) {
        if let Some(e) = self.entity_mut(entity) {
            e.effects.retain(|eff| eff.kind != kind);
        }
    }

    fn revive(&mut self, entity: i64, life: i64) {
        if let Some(e) = self.entity_mut(entity)
            && e.life <= 0
        {
            e.life = life.clamp(1, e.max_life);
        }
    }

    fn cooldown(&self, entity: i64, item: i64) -> i64 {
        self.get(entity).and_then(|e| e.item_cooldowns.get(&item).copied()).unwrap_or(0)
    }
    fn uses_this_turn(&self, entity: i64, item: i64) -> i64 {
        self.get(entity).and_then(|e| e.item_uses.get(&item).copied()).unwrap_or(0)
    }
    fn register_use(&mut self, entity: i64, item: i64, cooldown: i64) {
        if let Some(e) = self.entity_mut(entity) {
            *e.item_uses.entry(item).or_insert(0) += 1;
            if cooldown > 0 {
                e.item_cooldowns.insert(item, cooldown);
            }
        }
    }
}

/// Shared fight handle: the AI's builtin calls and the generator driving the
/// fight observe the same mutable state.
pub type FightRef = Rc<RefCell<Fight>>;

/// Wrap a [`Fight`] in a shareable handle.
#[must_use]
pub fn shared(fight: Fight) -> FightRef {
    Rc::new(RefCell::new(fight))
}

/// Bridges the native backend's game-runtime hook to the fight functions,
/// dispatching against the shared [`Fight`] as the [`GameHost`].
struct FightRuntime(FightRef);

impl leek_backend_native::GameRuntime for FightRuntime {
    fn call(&mut self, name: &str, args: &[Value]) -> Value {
        call_game_builtin(&mut *self.0.borrow_mut(), name, args)
    }
}

/// Launch one AI under explicit [`NativeOptions`]: run its compiled `hir`
/// against `fight` (the fight's current entity is the subject), with the fight
/// builtins linked in. Returns the AI's value.
///
/// The caller chooses the options — pass `NativeOptions::release()…` for a
/// normal fight or `NativeOptions::debug()…with_debug_hooks(true)` to run the
/// AI under the debugger. `opts` is expected to have `with_link_game(true)`.
///
/// # Errors
/// Propagates a [`NativeError`] if the AI isn't in the native subset.
pub fn run_ai_with(fight: &FightRef, hir: &HirFile, opts: &NativeOptions) -> Result<Value, NativeError> {
    leek_backend_native::set_game_runtime(Some(Box::new(FightRuntime(fight.clone()))));
    let result = leek_backend_native::run(hir, opts);
    leek_backend_native::set_game_runtime(None);
    result
}

/// Launch one AI with the default release profile. Convenience wrapper over
/// [`run_ai_with`].
///
/// # Errors
/// Propagates a [`NativeError`] if the AI isn't in the native subset.
pub fn run_ai(
    fight: &FightRef,
    hir: &HirFile,
    version: u8,
    strict: bool,
) -> Result<Value, NativeError> {
    let opts = NativeOptions::release()
        .with_lang(version, strict)
        .with_link_game(true);
    run_ai_with(fight, hir, &opts)
}

/// How a fight ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The lone surviving team, or `None` for a draw (no survivors, or the
    /// turn limit was hit with multiple teams alive).
    pub winner_team: Option<i64>,
    /// Turns played.
    pub turns: u32,
}

/// The turn loop, generic over how an entity's AI **and its run options** are
/// looked up. Each turn, every living entity (in id order) regenerates MP/TP,
/// ticks effects, and runs its AI once under the options `get_ai` returns for
/// it. Stops when at most one team remains or after `max_turns`. Entities for
/// which `get_ai` returns `None` act only as targets. Returning per-entity
/// options lets the debugger run one entity with debug hooks and the rest
/// without (see [`run_fight_debug`]).
fn fight_loop<'a>(
    fight: &FightRef,
    max_turns: u32,
    get_ai: impl Fn(i64) -> Option<(&'a HirFile, &'a NativeOptions)>,
) -> Result<Outcome, NativeError> {
    for turn in 1..=max_turns {
        fight.borrow_mut().turn = i64::from(turn);
        let order: Vec<i64> = {
            let mut ids = fight.borrow().entities(true);
            ids.sort_unstable();
            ids
        };
        for id in order {
            // Skip entities killed earlier this turn.
            if fight.borrow().life(id).is_none_or(|l| l <= 0) {
                continue;
            }
            {
                let mut f = fight.borrow_mut();
                f.set_current(id);
                f.regen(id);
                f.tick_effects(id); // poison damage + expire shields/buffs
            }
            // Poison may have killed the entity before it acts.
            if fight.borrow().life(id).is_none_or(|l| l <= 0) {
                continue;
            }
            if let Some((hir, opts)) = get_ai(id) {
                run_ai_with(fight, hir, opts)?;
            }
            if fight.borrow().living_teams().len() <= 1 {
                return Ok(Outcome {
                    winner_team: fight.borrow().living_teams().first().copied(),
                    turns: turn,
                });
            }
        }
    }
    Ok(Outcome {
        winner_team: fight
            .borrow()
            .living_teams()
            .first()
            .copied()
            .filter(|_| fight.borrow().living_teams().len() == 1),
        turns: max_turns,
    })
}

/// Run the fight to a conclusion with the default release profile. Convenience
/// wrapper over [`run_fight_with`] (see [`fight_loop`] for the turn semantics).
///
/// # Errors
/// Propagates the first [`NativeError`] from launching an AI.
pub fn run_fight(
    fight: &FightRef,
    ais: &HashMap<i64, HirFile>,
    max_turns: u32,
    version: u8,
    strict: bool,
) -> Result<Outcome, NativeError> {
    let opts = NativeOptions::release()
        .with_lang(version, strict)
        .with_link_game(true);
    fight_loop(fight, max_turns, |id| ais.get(&id).map(|h| (h, &opts)))
}

/// Run the fight to a conclusion under explicit [`NativeOptions`], with AIs
/// shared via [`Arc`] (so callers — the matrix runner, the debugger — can hold
/// the compiled HIR across constructions without cloning it). `opts` is
/// expected to have `with_link_game(true)` and the desired language/version.
///
/// # Errors
/// Propagates the first [`NativeError`] from launching an AI.
pub fn run_fight_with(
    fight: &FightRef,
    ais: &HashMap<i64, Arc<HirFile>>,
    max_turns: u32,
    opts: &NativeOptions,
) -> Result<Outcome, NativeError> {
    fight_loop(fight, max_turns, |id| {
        ais.get(&id).map(|a| (a.as_ref(), opts))
    })
}

/// Run a fight with the default release profile and [`Arc`]-shared AIs. The
/// convenience wrapper most callers (the scenario runner, the matrix/tournament
/// drivers) want: it builds the standard release [`NativeOptions`] with the
/// game builtins linked and delegates to [`run_fight_with`].
///
/// # Errors
/// Propagates the first [`NativeError`] from launching an AI.
pub fn run_fight_release(
    fight: &FightRef,
    ais: &HashMap<i64, Arc<HirFile>>,
    max_turns: u32,
    version: u8,
    strict: bool,
) -> Result<Outcome, NativeError> {
    let opts = NativeOptions::release()
        .with_lang(version, strict)
        .with_link_game(true);
    run_fight_with(fight, ais, max_turns, &opts)
}

/// Run a fight where a single entity is debugged: `debug_entity`'s AI runs under
/// `debug_opts` (expected to carry `with_debug_hooks(true)`), every other AI
/// under `other_opts`. Because only the debugged AI is compiled with debug
/// hooks, only it emits safepoints — so the process-global debug hook fires for
/// that entity alone, keeping breakpoints scoped to the AI under test even
/// though all AIs share the loop.
///
/// # Errors
/// Propagates the first [`NativeError`] from launching an AI.
pub fn run_fight_debug(
    fight: &FightRef,
    ais: &HashMap<i64, Arc<HirFile>>,
    max_turns: u32,
    debug_entity: i64,
    debug_opts: &NativeOptions,
    other_opts: &NativeOptions,
) -> Result<Outcome, NativeError> {
    fight_loop(fight, max_turns, |id| {
        ais.get(&id).map(|a| {
            let opts = if id == debug_entity {
                debug_opts
            } else {
                other_opts
            };
            (a.as_ref(), opts)
        })
    })
}
