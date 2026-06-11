//! The official fight state — ports of the reference `state/Entity.java`
//! (fight-relevant core), `state/Team.java` and `state/Order.java`.
//!
//! Java's `Entity` is a web of back-references (`entity.state`,
//! `effect.caster`, `effect.target`); in Rust the graph is flattened into
//! arenas: fighters are indexed by **fight id** (`fid`, their index in the
//! state's fighter list, like `State.addEntity`'s sequential ids) and
//! effects by their arena index, with `Fighter::effects` /
//! `Fighter::launched_effects` holding indices instead of references. The
//! `State` container itself (turn loop, movement, attack application) is
//! integrated on top of [`crate::map`] in a follow-up step.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::collections::HashMap;

use serde_json::{Value, json};

use crate::actions::{Action, ActionLog};
use crate::attack::{
    Area, AttackType, EffectInstance, EffectParams, EffectType, EntityState, java_round,
};
use crate::map::Map;
use crate::outcome::{entity_snapshot, map_json};
use crate::rng::OfficialRng;

// ─────────────────────────────────────────────────────────────────────────────
// Stats  (leek/Stats.java)
// ─────────────────────────────────────────────────────────────────────────────

/// Stat ids (`Entity.STAT_*`). Ids 7 and 8 are unused in the reference.
pub const STAT_LIFE: usize = 0;
pub const STAT_TP: usize = 1;
pub const STAT_MP: usize = 2;
pub const STAT_STRENGTH: usize = 3;
pub const STAT_AGILITY: usize = 4;
pub const STAT_FREQUENCY: usize = 5;
pub const STAT_WISDOM: usize = 6;
pub const STAT_ABSOLUTE_SHIELD: usize = 9;
pub const STAT_RELATIVE_SHIELD: usize = 10;
pub const STAT_RESISTANCE: usize = 11;
pub const STAT_SCIENCE: usize = 12;
pub const STAT_MAGIC: usize = 13;
pub const STAT_DAMAGE_RETURN: usize = 14;
pub const STAT_POWER: usize = 15;
pub const STAT_CORES: usize = 16;
pub const STAT_RAM: usize = 17;

/// `Stats.SIZE` — the number of stat slots (`STAT_RAM + 1`).
pub const STAT_COUNT: usize = 18;

/// A characteristics vector indexed by `STAT_*` id (`leek/Stats.java`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats([i32; STAT_COUNT]);

impl Stats {
    /// `Stats.getStat(id)`.
    #[must_use]
    pub fn get(&self, id: usize) -> i32 {
        self.0[id]
    }

    /// `Stats.setStat(id, value)`.
    pub fn set(&mut self, id: usize, value: i32) {
        self.0[id] = value;
    }

    /// `Stats.updateStat(id, delta)`.
    pub fn update(&mut self, id: usize, delta: i32) {
        self.0[id] += delta;
    }

    /// `Stats.addStats(stats)` — accumulate another vector.
    pub fn add(&mut self, other: &Self) {
        for (s, o) in self.0.iter_mut().zip(other.0.iter()) {
            *s += o;
        }
    }

    /// `Stats.clear()`.
    pub fn clear(&mut self) {
        self.0 = [0; 18];
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Fighter  (state/Entity.java, fight-relevant core, leek scope)
// ─────────────────────────────────────────────────────────────────────────────

/// `Entity.SAY_LIMIT_TURN`.
pub const SAY_LIMIT_TURN: i32 = 2;
/// `Entity.SHOW_LIMIT_TURN`.
pub const SHOW_LIMIT_TURN: i32 = 5;

/// `State.MAX_TURNS`.
pub const MAX_TURNS: i32 = 64;

/// `State.SUMMON_LIMIT` — max *alive* summons per team.
pub const SUMMON_LIMIT: usize = 8;

/// `AILog.SSTANDARD` — system-log level a `LeekLog.STANDARD` log is
/// upgraded to by `EntityAI.addSystemLog`.
pub const LOG_SSTANDARD: i32 = 6;
/// `AILog.SWARNING` — system-log level a `LeekLog.WARNING` log is
/// upgraded to by `EntityAI.addSystemLog`.
pub const LOG_SWARNING: i32 = 7;
/// `FarmerLog.BULB_WITHOUT_AI`.
pub const FARMER_LOG_BULB_WITHOUT_AI: i32 = 1005;
/// `Error.HELP_PAGE_LINK.ordinal()`.
pub const ERROR_HELP_PAGE_LINK: i32 = 113;

/// One fighting entity — the fight-relevant core of `Entity.java`, scoped to
/// leeks (no summons / turrets / chests yet). Mutations that need the rest
/// of the world (logging, damage returns, dying) live on the state
/// container; `Fighter` holds the data plus self-contained operations.
#[derive(Debug, Clone)]
pub struct Fighter {
    /// Index in the state's fighter arena (`Entity.getFId()`).
    pub fid: usize,
    /// Real entity id (`Entity.getId()` — scenario-provided, used in logs).
    pub id: i64,
    pub name: String,
    pub level: i32,
    /// 0-based team index (`Entity.getTeam()`).
    pub team: usize,
    pub farmer: i64,
    pub ai_name: String,

    /// `mBaseStats` — scenario characteristics.
    pub base_stats: Stats,
    /// `mBuffStats` — rebuilt from active effects by `update_buff_stats`.
    pub buff_stats: Stats,

    /// Current life (`life`).
    pub life: i32,
    /// Max life (`mTotalLife`) — eroded by nova damage, raised by vitality.
    pub total_life: i32,
    /// Max life at fight start (`mInitialLife`).
    pub initial_life: i32,

    /// TP consumed this turn (`usedTP`).
    pub used_tp: i32,
    /// MP consumed this turn (`usedMP`).
    pub used_mp: i32,

    /// Current cell id, `None` when off-board (dead).
    pub cell: Option<usize>,

    /// Owned weapon template ids (`mWeapons`).
    pub weapons: Vec<i32>,
    /// Equipped weapon template id (`weapon`).
    pub weapon: Option<i32>,
    /// Owned chip template ids (`mChips` — TreeMap, so ordered).
    pub chips: BTreeSet<i32>,
    /// Per-chip cooldowns (`mCooldown` — TreeMap, so ordered).
    pub cooldowns: BTreeMap<i32, i32>,
    /// Uses per item this turn (`itemUses`).
    pub item_uses: HashMap<i32, i32>,

    /// Indices of effects currently *on* this fighter (`effects`).
    pub effects: Vec<usize>,
    /// Indices of effects this fighter has *cast* (`launchedEffects`).
    pub launched_effects: Vec<usize>,
    /// Entity states pinned by `EffectAddState` effects (`states`) — added on
    /// apply, rebuilt from live effects by `State::update_buff_stats`.
    pub states: Vec<EntityState>,

    /// `saysTurn` / `showsTurn` — per-turn action caps.
    pub says_turn: i32,
    pub shows_turn: i32,

    /// Operations consumed across the fight (`totalOperations`).
    pub total_operations: i64,

    /// The summoner's fid for a bulb (`Bulb.mOwner`); `None` for leeks.
    /// `is_summon()` derives from it.
    pub summoner: Option<usize>,
    /// `mBirthTurn` — the turn this entity was summoned (0 for leeks).
    pub birth_turn: i32,
    /// `mSkin` — bulbs carry their template id; leeks default to 0.
    pub skin: i32,
}

impl Fighter {
    /// A leek with the given scenario characteristics, full life, nothing
    /// equipped (`Leek` constructor + `setTotalLife` + `startFight`).
    #[must_use]
    pub fn new(fid: usize, id: i64, name: String, team: usize, base_stats: Stats) -> Self {
        let life = base_stats.get(STAT_LIFE);
        Self {
            fid,
            id,
            name,
            level: 1,
            team,
            farmer: 0,
            ai_name: String::new(),
            base_stats,
            buff_stats: Stats::default(),
            life,
            total_life: life,
            initial_life: life,
            used_tp: 0,
            used_mp: 0,
            cell: None,
            weapons: Vec::new(),
            weapon: None,
            chips: BTreeSet::new(),
            cooldowns: BTreeMap::new(),
            item_uses: HashMap::new(),
            effects: Vec::new(),
            launched_effects: Vec::new(),
            states: Vec::new(),
            says_turn: 0,
            shows_turn: 0,
            total_operations: 0,
            summoner: None,
            birth_turn: 0,
            skin: 0,
        }
    }

    /// `Entity.isSummon()` — true for bulbs.
    #[must_use]
    pub fn is_summon(&self) -> bool {
        self.summoner.is_some()
    }

    /// `Entity.getStat(id)` — base + buff.
    #[must_use]
    pub fn stat(&self, id: usize) -> i32 {
        self.base_stats.get(id) + self.buff_stats.get(id)
    }

    /// `Entity.isDead()`.
    #[must_use]
    pub fn is_dead(&self) -> bool {
        self.life <= 0
    }

    /// `Entity.hasState(state)`.
    #[must_use]
    pub fn has_state(&self, state: EntityState) -> bool {
        self.states.contains(&state)
    }

    /// `Entity.getTP()` — total minus used.
    #[must_use]
    pub fn tp(&self) -> i32 {
        self.stat(STAT_TP) - self.used_tp
    }

    /// `Entity.getMP()` — total minus used.
    #[must_use]
    pub fn mp(&self) -> i32 {
        self.stat(STAT_MP) - self.used_mp
    }

    /// `Entity.useTP(n)`.
    pub fn use_tp(&mut self, n: i32) {
        self.used_tp += n;
    }

    /// `Entity.useMP(n)`.
    pub fn use_mp(&mut self, n: i32) {
        self.used_mp += n;
    }

    /// `Entity.hasWeapon(id)`.
    #[must_use]
    pub fn has_weapon(&self, id: i32) -> bool {
        self.weapons.contains(&id)
    }

    /// `Entity.getItemUses(id)`.
    #[must_use]
    pub fn item_uses(&self, id: i32) -> i32 {
        self.item_uses.get(&id).copied().unwrap_or(0)
    }

    /// `Entity.addItemUse(id)`.
    pub fn add_item_use(&mut self, id: i32) {
        *self.item_uses.entry(id).or_insert(0) += 1;
    }

    /// `Entity.addCooldown(chip, cooldown)` — `-1` means "rest of the fight".
    pub fn add_cooldown(&mut self, chip: i32, cooldown: i32) {
        let v = if cooldown == -1 {
            MAX_TURNS + 2
        } else {
            cooldown
        };
        self.cooldowns.insert(chip, v);
    }

    /// `Entity.hasCooldown(chipID)`.
    #[must_use]
    pub fn has_cooldown(&self, chip: i32) -> bool {
        self.cooldowns.contains_key(&chip)
    }

    /// `Entity.getCooldown(chipID)` — 0 when none.
    #[must_use]
    pub fn cooldown(&self, chip: i32) -> i32 {
        self.cooldowns.get(&chip).copied().unwrap_or(0)
    }

    /// `Entity.applyCoolDown` / `Entity.decrementOrRemove` — decrement every
    /// cooldown by 1, removing entries that reach 0.
    pub fn apply_cooldown(&mut self) {
        decrement_or_remove(&mut self.cooldowns);
    }

    /// `Entity.endTurn` — reset the per-turn counters. (The propagation step
    /// joins when `TYPE_PROPAGATION` effects are ported.)
    pub fn end_turn(&mut self) {
        self.used_mp = 0;
        self.used_tp = 0;
        self.says_turn = 0;
        self.shows_turn = 0;
        self.item_uses.clear();
    }
}

/// `Entity.decrementOrRemove` — shared by entity and team cooldowns.
pub fn decrement_or_remove(cooldowns: &mut BTreeMap<i32, i32>) {
    cooldowns.retain(|_, v| {
        if *v <= 1 {
            false
        } else {
            *v -= 1;
            true
        }
    });
}

// ─────────────────────────────────────────────────────────────────────────────
// Team  (state/Team.java)
// ─────────────────────────────────────────────────────────────────────────────

/// One team: its fighters (by fid) and team-level chip cooldowns.
#[derive(Debug, Clone, Default)]
pub struct Team {
    /// Real team id (`Team.getID()` — scenario-provided).
    pub id: i64,
    /// Member fids, in scenario order (`entities`).
    pub fighters: Vec<usize>,
    /// Team-level cooldowns (summon chips etc. — `cooldowns`).
    pub cooldowns: BTreeMap<i32, i32>,
}

impl Team {
    /// `Team.isDead()` — leek scope: dead when no member is alive (the
    /// turret/chest special cases apply to fight types out of scope).
    #[must_use]
    pub fn is_dead(&self, fighters: &[Fighter]) -> bool {
        self.fighters.iter().all(|&f| fighters[f].is_dead())
    }

    /// `Team.getLife()` — summed member life, **summons excluded** (the
    /// draw life-tiebreak only counts leeks).
    #[must_use]
    pub fn life(&self, fighters: &[Fighter]) -> i32 {
        self.fighters
            .iter()
            .filter(|&&f| !fighters[f].is_summon())
            .map(|&f| fighters[f].life)
            .sum()
    }

    /// `Team.getSummonCount()` — alive summons on this team.
    #[must_use]
    pub fn summon_count(&self, fighters: &[Fighter]) -> usize {
        self.fighters
            .iter()
            .filter(|&&f| !fighters[f].is_dead() && fighters[f].is_summon())
            .count()
    }

    /// `Team.addCooldown(chip, cooldown)`.
    pub fn add_cooldown(&mut self, chip: i32, cooldown: i32) {
        let v = if cooldown == -1 {
            MAX_TURNS + 2
        } else {
            cooldown
        };
        self.cooldowns.insert(chip, v);
    }

    /// `Team.hasCooldown(chipID)`.
    #[must_use]
    pub fn has_cooldown(&self, chip: i32) -> bool {
        self.cooldowns.contains_key(&chip)
    }

    /// `Team.getCooldown(chipID)` — 0 when none.
    #[must_use]
    pub fn cooldown(&self, chip: i32) -> i32 {
        self.cooldowns.get(&chip).copied().unwrap_or(0)
    }

    /// `Team.applyCoolDown()`.
    pub fn apply_cooldown(&mut self) {
        decrement_or_remove(&mut self.cooldowns);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Order  (state/Order.java)
// ─────────────────────────────────────────────────────────────────────────────

/// The play order: fids in turn order plus the current position and turn
/// counter. Bit-exact port of `Order.java`, including the position fixups
/// when entities die mid-round.
///
/// The position is an `i32` (transiently `-1` inside `remove_entity`) and
/// indices convert through Java-int semantics; sizes are bounded by the
/// entity count, so the casts are lossless.
#[derive(Debug, Clone)]
pub struct Order {
    fids: Vec<usize>,
    position: i32,
    turn: i32,
}

impl Default for Order {
    fn default() -> Self {
        Self::new()
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]
impl Order {
    #[must_use]
    pub fn new() -> Self {
        Self {
            fids: Vec::new(),
            position: 0,
            turn: 1,
        }
    }

    /// `Order.addEntity(leek)` — append (initial order construction).
    /// `Order.addSummon(owner, invoc)` — insert right after the owner, with
    /// **no position fixup** (Java mutates the list directly here, unlike
    /// `addEntity(index, …)`; the owner is the current entity, so the
    /// insertion point is always past the cursor). A missing owner inserts
    /// nothing.
    pub fn add_summon(&mut self, owner: usize, fid: usize) {
        if let Some(idx) = self.fids.iter().position(|&f| f == owner) {
            self.fids.insert(idx + 1, fid);
        }
    }

    pub fn add_entity(&mut self, fid: usize) {
        self.fids.push(fid);
    }

    /// `Order.addEntity(index, invoc)` — insert (summons), shifting the
    /// current position when inserting at or before it.
    pub fn insert_entity(&mut self, index: usize, fid: usize) {
        self.fids.insert(index, fid);
        if index as i32 <= self.position {
            self.position += 1;
        }
    }

    /// `Order.removeEntity(leek)` — remove a dead entity, fixing up the
    /// position (and rolling the turn back when the current entity at
    /// position 0 is removed, since `next()` will re-increment it).
    pub fn remove_entity(&mut self, fid: usize) {
        let Some(index) = self.fids.iter().position(|&f| f == fid) else {
            return;
        };
        if index as i32 <= self.position {
            self.position -= 1;
        }
        self.fids.remove(index);
        if self.position == -1 {
            self.position = self.fids.len() as i32 - 1;
            self.turn -= 1;
        }
    }

    /// `Order.current()` — `None` when the order is empty / out of range.
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        if self.position < 0 || self.fids.len() as i32 <= self.position {
            return None;
        }
        Some(self.fids[self.position as usize])
    }

    /// `Order.getTurn()`.
    #[must_use]
    pub fn turn(&self) -> i32 {
        self.turn
    }

    /// `Order.getPosition()`.
    #[must_use]
    pub fn position(&self) -> i32 {
        self.position
    }

    /// `Order.next()` — advance; returns `true` (and increments the turn)
    /// when the round wrapped.
    #[allow(clippy::should_implement_trait)] // named after Order.next()
    pub fn next(&mut self) -> bool {
        self.position += 1;
        if self.position >= self.fids.len() as i32 {
            self.turn += 1;
            self.position %= self.fids.len() as i32;
            return true;
        }
        false
    }

    /// `Order.getEntities()`.
    #[must_use]
    pub fn fids(&self) -> &[usize] {
        &self.fids
    }

    /// `Order.getEntityTurnOrder(e)` — 1-based position, 0 if absent.
    #[must_use]
    pub fn entity_turn_order(&self, fid: usize) -> i32 {
        self.fids
            .iter()
            .position(|&f| f == fid)
            .map_or(0, |i| i as i32 + 1)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Attack use results  (attack/Attack.java)
// ─────────────────────────────────────────────────────────────────────────────

/// `Attack.USE_*` result codes, as returned by `useWeapon`/`useChip`.
pub const USE_CRITICAL: i32 = 2;
pub const USE_SUCCESS: i32 = 1;
pub const USE_FAILED: i32 = 0;
pub const USE_INVALID_TARGET: i32 = -1;
pub const USE_NOT_ENOUGH_TP: i32 = -2;
pub const USE_INVALID_COOLDOWN: i32 = -3;
pub const USE_INVALID_POSITION: i32 = -4;
pub const USE_TOO_MANY_SUMMONS: i32 = -5;
pub const USE_RESURRECT_INVALID_ENTITY: i32 = -6;
pub const USE_MAX_USES: i32 = -7;

/// The use-rules and effects of a weapon template (`weapons/Weapon.java` +
/// `attack/Attack.java` accessors).
#[derive(Debug, Clone)]
pub struct WeaponSpec {
    pub id: i32,
    pub cost: i32,
    pub min_range: i32,
    pub max_range: i32,
    /// `Attack.getLaunchType()` bits (1 = line, 2 = diagonal, 4 = other).
    pub launch_type: i32,
    pub needs_los: bool,
    /// `Attack.getMaxUses()` — `-1` is unlimited; `0` blocks every use
    /// (`getItemUses >= 0` holds immediately).
    pub max_uses: i32,
    /// `Attack.getArea()`.
    pub area: Area,
    /// The attack's effect lines (`Attack.getEffects()`).
    pub effects: Vec<EffectParams>,
}

/// The use-rules, effects and cooldown data of a chip template
/// (`chips/Chip.java` + `attack/Attack.java` accessors).
#[derive(Debug, Clone)]
pub struct ChipSpec {
    pub id: i32,
    pub cost: i32,
    pub min_range: i32,
    pub max_range: i32,
    /// `Attack.getLaunchType()` bits (1 = line, 2 = diagonal, 4 = other).
    pub launch_type: i32,
    pub needs_los: bool,
    /// `Attack.getMaxUses()` — `-1` is unlimited.
    pub max_uses: i32,
    /// `Attack.getArea()`.
    pub area: Area,
    /// The attack's effect lines (`Attack.getEffects()`).
    pub effects: Vec<EffectParams>,
    /// `Chip.getCooldown()` — `0` is none, `-1` is "rest of the fight".
    pub cooldown: i32,
    /// `Chip.isTeamCooldown()` — cooldown lives on the team, not the entity.
    pub team_cooldown: bool,
    /// `Chip.getInitialCooldown()` — pre-charged for everyone at fight start.
    pub initial_cooldown: i32,
    /// `Chip.getLevel()` — copied onto bulbs summoned by this chip
    /// (`createSummon(…, template.getLevel(), …)`).
    pub level: i32,
}

/// One bulb template (`bulbs/BulbTemplate.java` — `data/summons.json` entry):
/// `(min, max)` stat ranges scaled by the summoner's level, plus the chips
/// granted to the bulb.
#[derive(Debug, Clone)]
pub struct BulbTemplate {
    pub id: i32,
    pub name: String,
    pub life: (i32, i32),
    pub strength: (i32, i32),
    pub wisdom: (i32, i32),
    pub agility: (i32, i32),
    pub resistance: (i32, i32),
    pub science: (i32, i32),
    pub magic: (i32, i32),
    pub tp: (i32, i32),
    pub mp: (i32, i32),
    /// Chip template ids, in `summons.json` order — `Entity.addChip` caps at
    /// the bulb's RAM (6), so only the first 6 stick.
    pub chips: Vec<i32>,
}

/// `BulbTemplate.base(base, bonus, coeff, multiplier)` — the bulb stat
/// formula: `(int) ((min + Math.floor((max - min) * coeff)) * multiplier)`.
/// The cast truncates toward zero like Java's `(int)`.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
fn bulb_base((min, max): (i32, i32), coeff: f64, multiplier: f64) -> i32 {
    ((f64::from(min) + (f64::from(max - min) * coeff).floor()) * multiplier) as i32
}

// ─────────────────────────────────────────────────────────────────────────────
// State  (state/State.java + the Fight.java turn loop, leek scope)
// ─────────────────────────────────────────────────────────────────────────────

/// What `Fight.startTurn` should do after the turn-start phase
/// (see [`State::begin_turn`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginTurn {
    /// `order.current()` is null — the orchestrator finishes the fight.
    NoCurrent,
    /// The current entity died during its turn start: skip its AI (and its
    /// `ActionEndTurn`), but still run [`State::end_turn`].
    Skip,
    /// Run this entity's AI, then [`State::end_entity_turn`] +
    /// [`State::end_turn`].
    Act(usize),
}

/// The official fight world — `State.java`'s fight-relevant core plus the
/// pieces of `Fight.java`'s loop that mutate it. The AI execution itself
/// lives in the generator crate; this container exposes the exact
/// `Fight.startTurn` phases ([`State::begin_turn`] /
/// [`State::end_entity_turn`] / [`State::end_turn`]) so the orchestrator can
/// interleave AI runs bit-exactly.
///
/// Entity occupancy is tracked here (`occupancy`), not on [`Map`]: the map
/// port is entity-blind, so the pathfinding wrappers reproduce the
/// occupancy-dependent quirks of `Map.getAStarPath` (currently the
/// last-cell strip — exact for 1v1 fights, where no third entity can block
/// a path interior; full occupancy-aware A* integrates when the map port
/// lands entity support).
pub struct State {
    pub rng: OfficialRng,
    pub map: Map,
    /// Fighter arena, indexed by fid (`State.mEntities`, dense).
    pub fighters: Vec<Fighter>,
    pub teams: Vec<Team>,
    pub order: Order,
    pub actions: ActionLog,
    /// Registered weapon templates (`Weapons.getTemplates()`).
    pub weapon_specs: BTreeMap<i32, WeaponSpec>,
    /// Registered chip templates (`Chips.getTemplates()` — TreeMap order).
    pub chip_specs: BTreeMap<i32, ChipSpec>,
    /// Registered bulb templates (`Bulbs.getInvocationTemplate`).
    pub bulb_templates: BTreeMap<i32, BulbTemplate>,
    /// The AI function value passed to `summon()`, keyed by the bulb's fid
    /// (`BulbAI.mAIFunction`). Absent for `useChip`-summoned bulbs
    /// (BULB_WITHOUT_AI) — they idle. The orchestrator dispatches these
    /// through the owner's re-JIT'd module each bulb turn.
    pub summon_ais: HashMap<usize, leek_runtime::Value>,
    /// Live effect instances — an arena, never compacted: the fighters'
    /// `effects` / `launched_effects` index lists are the membership truth.
    pub effects: Vec<EffectInstance>,
    /// cell id → fid of the entity standing on it (`Cell.getPlayer`).
    occupancy: HashMap<usize, usize>,
    /// `mState == STATE_RUNNING`.
    pub running: bool,
    /// `State.lastTurn` — dedupes `ActionNewTurn` when a death rolled the
    /// turn counter back.
    last_turn: i32,
    /// All fids in start order (`State.initialOrder`) — the `fight.leeks`
    /// snapshot order.
    pub initial_order: Vec<usize>,
    /// Entity snapshots captured by [`State::record_initial_state`].
    pub leek_snapshots: Vec<Value>,
    /// Map JSON captured by [`State::record_initial_state`]
    /// (`Actions.addMap` runs before obstacles can be destroyed).
    pub map_snapshot: Value,
    /// Per-farmer system-log buffers (`FarmerLog.mObject`) — farmer id →
    /// last-action-id key → entries appended while that action was current.
    /// Serialized into the Outcome's `logs` object by `build_outcome`.
    pub farmer_logs: BTreeMap<i64, BTreeMap<usize, Vec<Value>>>,
}

impl State {
    /// An empty world seeded like `state.seed(seed)`; entities and teams are
    /// added before [`State::init`].
    #[must_use]
    pub fn new(seed: i64) -> Self {
        Self {
            rng: OfficialRng::new(seed),
            map: Map::new(18, 18),
            fighters: Vec::new(),
            teams: Vec::new(),
            order: Order::new(),
            actions: ActionLog::new(),
            weapon_specs: BTreeMap::new(),
            chip_specs: BTreeMap::new(),
            bulb_templates: BTreeMap::new(),
            summon_ais: HashMap::new(),
            effects: Vec::new(),
            occupancy: HashMap::new(),
            running: false,
            last_turn: 0,
            initial_order: Vec::new(),
            leek_snapshots: Vec::new(),
            map_snapshot: Value::Null,
            farmer_logs: BTreeMap::new(),
        }
    }

    /// `State.addEntity(team, entity)` — assign the next fid, creating teams
    /// up to `team` as needed. Returns the fid.
    pub fn add_entity(&mut self, team: usize, mut fighter: Fighter) -> usize {
        while self.teams.len() <= team {
            let id = i64::try_from(self.teams.len()).expect("team count fits in i64");
            self.teams.push(Team {
                id,
                ..Team::default()
            });
        }
        let fid = self.fighters.len();
        fighter.fid = fid;
        fighter.team = team;
        self.teams[team].fighters.push(fid);
        self.fighters.push(fighter);
        fid
    }

    /// The living entity standing on `cell` (`Cell.getPlayer`).
    #[must_use]
    pub fn entity_on(&self, cell: usize) -> Option<usize> {
        self.occupancy.get(&cell).copied()
    }

    /// `Map.moveEntity(entity, cell)` — occupancy + entity position update.
    /// Keeps `map.entity_cells` (the A*'s `Cell.getPlayer` view) in sync.
    pub fn place_entity(&mut self, fid: usize, cell: usize) {
        if let Some(old) = self.fighters[fid].cell {
            self.occupancy.remove(&old);
            self.map.entity_cells.retain(|&c| c != old);
        }
        self.occupancy.insert(cell, fid);
        self.map.entity_cells.push(cell);
        self.fighters[fid].cell = Some(cell);
    }

    /// `Map.removeEntity(entity)` — clear occupancy (death).
    pub fn remove_entity_from_map(&mut self, fid: usize) {
        if let Some(cell) = self.fighters[fid].cell.take() {
            self.occupancy.remove(&cell);
            self.map.entity_cells.retain(|&c| c != cell);
        }
    }

    /// `State.slideEntity(entity, cell, caster)` — a forced move (push or
    /// attract). Updates the occupancy only; like Java, it produces NO action
    /// (the statistics manager is the sole observer there). The `onMoved`
    /// passives are weapon passive effects — none in the leek scope.
    pub fn slide_entity(&mut self, fid: usize, cell: usize) {
        // A STATIC entity cannot be pushed or attracted.
        if self.fighters[fid].has_state(EntityState::Static) {
            return;
        }
        if self.fighters[fid].cell == Some(cell) {
            return;
        }
        self.place_entity(fid, cell);
    }

    /// `State.teleportEntity(entity, cell, caster, itemId)` — like a slide,
    /// it only moves the occupancy; statistics-only in Java, no action.
    /// (Unlike the slides, Java has NO STATIC guard here — a static entity
    /// can still teleport.)
    pub fn teleport_entity(&mut self, fid: usize, cell: usize) {
        self.place_entity(fid, cell);
    }

    /// `State.invertEntities(caster, target)` — permutation: swap the two
    /// entities' cells. Occupancy-only like the slides (statistics-only in
    /// Java, no action logged). The `onMoved` passives are weapon passive
    /// effects — none in the leek scope.
    pub fn invert_entities(&mut self, a: usize, b: usize) {
        // Java checks ONLY the target for STATIC — a static caster still
        // swaps (and moves itself doing so).
        if self.fighters[b].has_state(EntityState::Static) {
            return;
        }
        let (Some(ca), Some(cb)) = (self.fighters[a].cell, self.fighters[b].cell) else {
            return;
        };
        self.occupancy.insert(ca, b);
        self.occupancy.insert(cb, a);
        self.fighters[a].cell = Some(cb);
        self.fighters[b].cell = Some(ca);
        // `map.entity_cells` already holds both cells — the swap doesn't
        // change the set.
    }

    /// `State.init()` — draw the obstacle count, generate the map, place the
    /// entities, compute the start order, pre-charge the initial chip
    /// cooldowns.
    ///
    /// # Panics
    /// Panics if the fight isn't the 2-team shape the map generator currently
    /// places (one leek per team).
    pub fn init(&mut self) {
        let obstacle_count = self.rng.get_int(30, 80);
        let (map, team0_cell, team1_cell) = Map::generate_map(&mut self.rng, obstacle_count);
        self.map = map;
        // `generate_map` pre-marks the spawn cells; `place_entity` below is
        // the single occupancy source from here on, so start from a clean set.
        self.map.entity_cells.clear();

        // Map.generateMap places one entity per team for now (1v1).
        assert!(
            self.teams.len() == 2 && self.teams.iter().all(|t| t.fighters.len() == 1),
            "official State currently supports 1v1 fights only"
        );
        if let Some(cell) = team0_cell {
            self.place_entity(self.teams[0].fighters[0], cell);
        }
        if let Some(cell) = team1_cell {
            self.place_entity(self.teams[1].fighters[0], cell);
        }

        // StartOrder.compute — teams of (fid, frequency).
        #[allow(clippy::cast_possible_wrap)]
        let start_teams: Vec<Vec<(i64, i64)>> = self
            .teams
            .iter()
            .map(|t| {
                t.fighters
                    .iter()
                    .map(|&f| (f as i64, i64::from(self.fighters[f].stat(STAT_FREQUENCY))))
                    .collect()
            })
            .collect();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        for fid in crate::order::compute_start_order(&start_teams, &mut self.rng) {
            let fid = fid as usize;
            if !self.fighters[fid].is_dead() {
                self.order.add_entity(fid);
            }
            self.initial_order.push(fid);
        }

        // Cooldowns initiaux — every registered chip with an initial cooldown
        // starts charged for every entity, at `initialCooldown + 1` (the +1
        // absorbs the entity's first start-of-turn tick).
        let initial: Vec<ChipSpec> = self
            .chip_specs
            .values()
            .filter(|c| c.initial_cooldown > 0)
            .cloned()
            .collect();
        for chip in &initial {
            for t in 0..self.teams.len() {
                for i in 0..self.teams[t].fighters.len() {
                    let fid = self.teams[t].fighters[i];
                    self.add_chip_cooldown(fid, chip, chip.initial_cooldown + 1);
                }
            }
        }

        self.running = true;
    }

    /// `State.recordInitialState()` — capture the `fight.leeks` snapshots and
    /// the map JSON, then log `ActionStartFight`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn record_initial_state(&mut self) {
        self.leek_snapshots = self
            .initial_order
            .iter()
            .map(|&f| entity_snapshot(&self.fighters[f], false))
            .collect();
        self.map_snapshot = map_json(&self.map);
        self.actions.log(Action::StartFight {
            team1: self.teams[0].fighters.len() as i32,
            team2: self.teams[1].fighters.len() as i32,
        });
    }

    /// `Fight.startTurn` up to the AI run: log `ActionEntityTurn` and run the
    /// entity's turn start, reporting what the orchestrator should do next.
    #[allow(clippy::cast_possible_wrap)]
    pub fn begin_turn(&mut self) -> BeginTurn {
        let Some(fid) = self.order.current() else {
            return BeginTurn::NoCurrent;
        };
        self.actions.log(Action::EntityTurn {
            entity_id: fid as i64,
        });
        self.start_turn(fid);
        if self.fighters[fid].is_dead() {
            BeginTurn::Skip
        } else {
            BeginTurn::Act(fid)
        }
    }

    /// `Entity.startTurn()` — tick the chip cooldowns, apply the start-turn
    /// effects sitting *on* this entity (poison ticks), then decrement and
    /// expire the effects this entity has *launched*.
    fn start_turn(&mut self, fid: usize) {
        self.fighters[fid].apply_cooldown();

        // Apply start-turn effects on a copy of the list (Java iterates an
        // `ArrayList` copy; ticks can mutate the live list via death).
        let effects_copy = self.fighters[fid].effects.clone();
        for ei in effects_copy {
            self.apply_start_turn_effect(ei);
            if self.fighters[fid].is_dead() {
                // Dying mid-tick skips the launched-effect expiry entirely.
                return;
            }
        }

        // Decrement the launched effects; remove the ones that expire.
        let mut i = 0;
        while i < self.fighters[fid].launched_effects.len() {
            let ei = self.fighters[fid].launched_effects[i];
            if self.effects[ei].turns != -1 {
                self.effects[ei].turns -= 1;
            }
            if self.effects[ei].turns == 0 {
                let target = self.effects[ei].target;
                self.remove_effect(target, ei);
                self.fighters[fid].launched_effects.remove(i);
            } else {
                i += 1;
            }
        }
    }

    /// The post-AI half of `Fight.startTurn`: `current.endTurn()` +
    /// `ActionEndTurn` (logged with the freshly reset TP/MP totals).
    #[allow(clippy::cast_possible_wrap)]
    pub fn end_entity_turn(&mut self, fid: usize) {
        self.fighters[fid].end_turn();
        self.propagate_effects(fid);
        self.actions.log(Action::EndTurn {
            entity_id: fid as i64,
            tp: i64::from(self.fighters[fid].tp()),
            mp: i64::from(self.fighters[fid].mp()),
        });
    }

    /// `State.isFinished()` — at most one team still alive (leek scope).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        let mut alive = 0;
        for team in &self.teams {
            if !team.is_dead(&self.fighters) {
                alive += 1;
                if alive >= 2 {
                    return false;
                }
            }
        }
        true
    }

    /// `State.endTurn()` — finish the fight, or advance the order (logging
    /// `ActionNewTurn` on a round wrap and ticking team cooldowns).
    pub fn end_turn(&mut self) {
        if self.is_finished() {
            self.running = false;
        } else if self.order.next() {
            let turn = self.order.turn();
            if self.last_turn != turn && turn <= MAX_TURNS {
                self.actions.log(Action::NewTurn { count: turn });
                self.last_turn = turn;
            }
            for team in &mut self.teams {
                team.apply_cooldown();
            }
        }
    }

    /// `Fight.computeWinner(drawCheckLife)` — the winning 0-based team index,
    /// `-1` for a draw. With `draw_check_life`, a draw is tie-broken by
    /// strictly-highest team life.
    #[must_use]
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn compute_winner(&self, draw_check_life: bool) -> i32 {
        let mut win_team = -1;
        let mut alive = 0;
        for (t, team) in self.teams.iter().enumerate() {
            if !team.is_dead(&self.fighters) {
                alive += 1;
                win_team = t as i32;
            }
        }
        if alive != 1 {
            win_team = -1;
        }
        if win_team == -1 && draw_check_life {
            let mut max_life = i32::MIN;
            let mut winners = 0;
            let mut candidate = -1;
            for (t, team) in self.teams.iter().enumerate() {
                let life = team.life(&self.fighters);
                if life > max_life {
                    max_life = life;
                    candidate = t as i32;
                    winners = 1;
                } else if life == max_life {
                    winners += 1;
                }
            }
            if winners == 1 {
                win_team = candidate;
            }
        }
        win_team
    }

    /// `State.getDuration()`.
    #[must_use]
    pub fn duration(&self) -> i32 {
        self.order.turn()
    }

    // ── Movement ─────────────────────────────────────────────────────────────

    /// `Map.getPathBetween(start, end, null)` — the entity-aware A*.
    /// `place_entity` keeps `map.entity_cells` synced with the live
    /// positions, so occupied cells block the path interior and the
    /// occupied-last-cell strip happens inside the map, exactly as in Java.
    pub fn path_between(&mut self, start: usize, end: usize) -> Option<Vec<usize>> {
        self.map.get_path_between(start, end, &[])
    }

    /// `State.moveEntity(entity, path)` — log the move, consume MP, update
    /// the position. Returns the cells moved.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn move_entity(&mut self, fid: usize, path: &[usize]) -> i64 {
        // A STATIC entity cannot move (checked before the size/MP gates —
        // no log, no MP spent).
        if self.fighters[fid].has_state(EntityState::Static) {
            return 0;
        }
        let size = path.len() as i32;
        if size == 0 || size > self.fighters[fid].mp() {
            return 0;
        }
        let end = *path.last().expect("non-empty path");
        self.actions.log(Action::Move {
            entity_id: fid as i64,
            end_cell: end as i32,
            path: path.iter().map(|&c| c as i32).collect(),
        });
        self.fighters[fid].use_mp(size);
        self.place_entity(fid, end);
        i64::from(size)
    }

    /// `State.moveToward(entity, leek_id, pm_to_use)` — path to a living
    /// entity's cell, truncated to the MP budget. `pm_to_use == -1` means
    /// "all remaining MP".
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn move_toward(&mut self, fid: usize, leek_id: i64, pm_to_use: i64) -> i64 {
        let mp = self.fighters[fid].mp();
        let mut pm = if pm_to_use == -1 {
            mp
        } else {
            pm_to_use as i32
        };
        if pm > mp {
            pm = mp;
        }
        if pm <= 0 {
            return 0;
        }
        let target = usize::try_from(leek_id)
            .ok()
            .filter(|&t| t < self.fighters.len());
        let Some(target) = target else { return 0 };
        if self.fighters[target].is_dead() {
            return 0;
        }
        let (Some(start), Some(end)) = (self.fighters[fid].cell, self.fighters[target].cell) else {
            return 0;
        };
        match self.path_between(start, end) {
            Some(path) => {
                // `pm > 0` was checked above.
                let take = path.len().min(usize::try_from(pm).unwrap_or(0));
                self.move_entity(fid, &path[..take])
            }
            None => 0,
        }
    }

    /// `State.moveTowardCell(entity, cell_id, pm_to_use)` — path to a cell;
    /// an unwalkable target paths to the valid cells around its obstacle
    /// cluster instead (`getValidCellsAroundObstacle`).
    #[allow(clippy::cast_possible_truncation)]
    pub fn move_toward_cell(&mut self, fid: usize, cell_id: i64, pm_to_use: i64) -> i64 {
        let mp = self.fighters[fid].mp();
        let mut pm = if pm_to_use == -1 {
            mp
        } else {
            pm_to_use as i32
        };
        if pm > mp {
            pm = mp;
        }
        if pm <= 0 {
            return 0;
        }
        let Some(start) = self.fighters[fid].cell else {
            return 0;
        };
        let target = i32::try_from(cell_id)
            .ok()
            .and_then(|c| self.map.get_cell(c));
        let Some(target) = target else { return 0 };
        if target == start {
            return 0;
        }
        let path = if self.map.cells[target].walkable {
            self.path_between(start, target)
        } else {
            let around = self.map.get_valid_cells_around_obstacle(target);
            self.map.get_astar_path(start, &around, &[])
        };
        match path {
            Some(path) => {
                // `pm > 0` was checked above.
                let take = usize::try_from(pm).unwrap_or(0).min(path.len());
                self.move_entity(fid, &path[..take])
            }
            None => 0,
        }
    }

    // ── Weapons ──────────────────────────────────────────────────────────────

    /// `State.setWeapon(entity, weapon)` — costs 1 TP, logs even when
    /// re-equipping the same weapon. Ownership is checked by the builtin
    /// wrapper (`EntityClass.setWeapon`), not here.
    pub fn set_weapon(&mut self, fid: usize, weapon: i32) -> bool {
        if self.fighters[fid].tp() <= 0 {
            return false;
        }
        self.fighters[fid].weapon = Some(weapon);
        self.fighters[fid].use_tp(1);
        self.actions.log(Action::SetWeapon {
            weapon_template: weapon,
        });
        true
    }

    /// `Map.canUseAttack(caster, target, attack)` — range then LOS. The
    /// first-in-line area pre-resolves its entity: aiming *at* it fails, and
    /// it is otherwise transparent to the LoS walk.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn can_use_attack(
        &self,
        caster_cell: usize,
        target_cell: usize,
        min_range: i32,
        max_range: i32,
        launch_type: i32,
        needs_los: bool,
        area: Area,
    ) -> bool {
        if !self
            .map
            .verify_range(caster_cell, target_cell, min_range, max_range, launch_type)
        {
            return false;
        }
        let mut ignored = vec![caster_cell];
        if area == Area::FirstInLine
            && let Some(cell) =
                self.map
                    .get_first_entity(caster_cell, target_cell, min_range, max_range)
        {
            if cell == target_cell {
                return false;
            }
            ignored.push(cell);
        }
        self.map
            .verify_los(caster_cell, target_cell, needs_los, &ignored)
    }

    /// `Fight.generateCritical(entity)`.
    pub fn generate_critical(&mut self, fid: usize) -> bool {
        self.rng.get_double() < f64::from(self.fighters[fid].stat(STAT_AGILITY)) / 1000.0
    }

    // ── Life and death ───────────────────────────────────────────────────────

    /// `Entity.removeLife(pv, erosion, attacker, type, effect, item)` —
    /// clamp, take the damage, erode max life, and handle death.
    /// (The statistics hooks aren't part of the Outcome.)
    pub fn remove_life(&mut self, target: usize, pv: i32, erosion: i32, attacker: Option<usize>) {
        if self.fighters[target].is_dead() {
            return;
        }
        let pv = pv.max(0);
        let erosion = erosion.max(0);
        let f = &mut self.fighters[target];
        f.life -= pv.min(f.life);
        f.total_life = (f.total_life - erosion).max(1);
        if f.life <= 0 {
            self.on_player_die(target, attacker);
            self.die(target);
        }
    }

    /// `Entity.addLife(healer, pv)` — heal, clamped to max life.
    pub fn add_life(&mut self, target: usize, pv: i32) {
        let f = &mut self.fighters[target];
        f.life += pv.clamp(0, f.total_life - f.life);
    }

    /// `State.onPlayerDie(entity, killer, item)` — pull the entity out of
    /// the order and off the map, log `ActionEntityDie`. (BR power transfer,
    /// chest loot and the ally-killed / kill passives are out of scope.)
    #[allow(clippy::cast_possible_wrap)]
    pub fn on_player_die(&mut self, fid: usize, killer: Option<usize>) {
        self.order.remove_entity(fid);
        self.remove_entity_from_map(fid);
        self.actions.log(Action::EntityDie {
            entity_id: fid as i64,
            killer_id: killer.map_or(-1, |k| k as i64),
        });
    }

    /// `Entity.die()` — zero the life, drop every effect in both directions,
    /// rebuild the buff stats, then kill the entity's summons.
    pub fn die(&mut self, fid: usize) {
        self.fighters[fid].life = 0;

        // Remove launched effects — `removeEffect` on each target *does* log
        // `ActionRemoveEffect`.
        while let Some(&ei) = self.fighters[fid].launched_effects.first() {
            let target = self.effects[ei].target;
            self.remove_effect(target, ei);
            self.fighters[fid].launched_effects.remove(0);
        }

        // Remove the effects on this entity — no remove-effect action, the
        // client removes the dead entity's effects itself.
        while let Some(&ei) = self.fighters[fid].effects.first() {
            let caster = self.effects[ei].caster;
            self.fighters[caster].launched_effects.retain(|&i| i != ei);
            self.fighters[fid].effects.remove(0);
        }
        self.update_buff_stats(fid);

        // Kill summons — `die()` ends by sweeping the dead entity's team for
        // *alive* summons it owns (`getTeamEntities(team)` filters dead) and
        // killing each: `onPlayerDie(e, null, null)` + recursive `die()`.
        let team = self.fighters[fid].team;
        let summons: Vec<usize> = self.teams[team]
            .fighters
            .iter()
            .copied()
            .filter(|&f| !self.fighters[f].is_dead() && self.fighters[f].summoner == Some(fid))
            .collect();
        for s in summons {
            self.on_player_die(s, None);
            self.die(s);
        }
    }

    /// `State.useWeapon(launcher, target)` — the exact check ladder, crit
    /// roll, logging, attack application, TP cost and use accounting.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn use_weapon(&mut self, fid: usize, target_cell: usize) -> i32 {
        if self.order.current() != Some(fid) {
            return USE_INVALID_TARGET;
        }
        let Some(weapon) = self.fighters[fid].weapon else {
            return USE_INVALID_TARGET;
        };
        let Some(spec) = self.weapon_specs.get(&weapon).cloned() else {
            return USE_INVALID_TARGET;
        };
        if spec.cost > self.fighters[fid].tp() {
            return USE_NOT_ENOUGH_TP;
        }
        if spec.max_uses != -1 && self.fighters[fid].item_uses(spec.id) >= spec.max_uses {
            return USE_MAX_USES;
        }
        let Some(caster_cell) = self.fighters[fid].cell else {
            return USE_INVALID_POSITION;
        };
        if !self.can_use_attack(
            caster_cell,
            target_cell,
            spec.min_range,
            spec.max_range,
            spec.launch_type,
            spec.needs_los,
            spec.area,
        ) {
            return USE_INVALID_POSITION;
        }

        let critical = self.generate_critical(fid);
        let result = if critical { USE_CRITICAL } else { USE_SUCCESS };
        self.actions.log(Action::UseWeapon {
            cell: target_cell as i32,
            success: result,
        });
        // launcher.onCritical(): weapon passive effects — none in leek scope.
        self.apply_on_cell(
            fid,
            target_cell,
            critical,
            spec.area,
            &spec.effects,
            spec.id,
            AttackType::Weapon,
            spec.min_range,
            spec.max_range,
            spec.needs_los,
        );

        self.fighters[fid].use_tp(spec.cost);
        self.fighters[fid].add_item_use(spec.id);
        result
    }

    // ── Chips ────────────────────────────────────────────────────────────────

    /// `State.addCooldown(entity, chip, cooldown)` — dispatch to the team or
    /// the entity depending on the chip.
    pub fn add_chip_cooldown(&mut self, fid: usize, chip: &ChipSpec, cooldown: i32) {
        if chip.team_cooldown {
            self.teams[self.fighters[fid].team].add_cooldown(chip.id, cooldown);
        } else {
            self.fighters[fid].add_cooldown(chip.id, cooldown);
        }
    }

    /// `State.hasCooldown(entity, chip)`.
    #[must_use]
    pub fn has_chip_cooldown(&self, fid: usize, chip: &ChipSpec) -> bool {
        if chip.team_cooldown {
            self.teams[self.fighters[fid].team].has_cooldown(chip.id)
        } else {
            self.fighters[fid].has_cooldown(chip.id)
        }
    }

    /// `State.getCooldown(entity, chip)` — 0 when none or unknown chip.
    #[must_use]
    pub fn chip_cooldown(&self, fid: usize, chip_id: i32) -> i32 {
        let Some(chip) = self.chip_specs.get(&chip_id) else {
            return 0;
        };
        if chip.team_cooldown {
            self.teams[self.fighters[fid].team].cooldown(chip.id)
        } else {
            self.fighters[fid].cooldown(chip.id)
        }
    }

    /// `State.useChip(caster, target, template)` — the exact check ladder
    /// (note the cost check is `cost > 0 && cost > TP`, unlike the weapon's),
    /// crit roll, logging, attack application, cooldown, TP cost and use
    /// accounting.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn use_chip(&mut self, fid: usize, target_cell: usize, chip_id: i32) -> i32 {
        let Some(spec) = self.chip_specs.get(&chip_id).cloned() else {
            return USE_INVALID_TARGET;
        };
        if self.order.current() != Some(fid) {
            return USE_INVALID_TARGET;
        }
        if spec.cost > 0 && spec.cost > self.fighters[fid].tp() {
            return USE_NOT_ENOUGH_TP;
        }
        if self.has_chip_cooldown(fid, &spec) {
            return USE_INVALID_COOLDOWN;
        }
        // Uses per turn.
        if spec.max_uses != -1 && self.fighters[fid].item_uses(spec.id) >= spec.max_uses {
            return USE_MAX_USES;
        }
        let Some(caster_cell) = self.fighters[fid].cell else {
            return USE_INVALID_POSITION;
        };
        if !self.map.cells[target_cell].walkable
            || !self.can_use_attack(
                caster_cell,
                target_cell,
                spec.min_range,
                spec.max_range,
                spec.launch_type,
                spec.needs_los,
                spec.area,
            )
        {
            return USE_INVALID_POSITION;
        }
        // Teleportation gets an extra cell-availability check per effect
        // (before the crit roll — a failed precheck draws no RNG and
        // returns USE_INVALID_TARGET, not USE_INVALID_POSITION).
        for params in &spec.effects {
            if params.effect == EffectType::Teleport
                && !(self.map.cells[target_cell].walkable && self.entity_on(target_cell).is_none())
            {
                return USE_INVALID_TARGET;
            }
        }

        let critical = self.generate_critical(fid);
        let result = if critical { USE_CRITICAL } else { USE_SUCCESS };
        self.actions.log(Action::UseChip {
            chip_template: spec.id,
            cell: target_cell as i32,
            success: result,
        });
        // caster.onCritical(): passive effects — none in leek scope.
        self.apply_on_cell(
            fid,
            target_cell,
            critical,
            spec.area,
            &spec.effects,
            spec.id,
            AttackType::Chip,
            spec.min_range,
            spec.max_range,
            spec.needs_los,
        );

        if spec.cooldown != 0 {
            self.add_chip_cooldown(fid, &spec, spec.cooldown);
        }
        self.fighters[fid].use_tp(spec.cost);
        self.fighters[fid].add_item_use(spec.id);
        result
    }

    // ── Summons ──────────────────────────────────────────────────────────────

    /// `State.summonEntity(caster, target, template, name)` — the exact check
    /// ladder (note: a plain `cost > TP` check, unlike `useChip`'s
    /// `cost > 0 &&` gate; no max-uses check; no `addItemUse`), crit roll,
    /// `ActionUseChip` + bulb creation + `ActionInvocation` logging, cooldown
    /// and TP cost. Returns the new bulb's fid alongside the result so the
    /// orchestrator can attach the AI function (`Fight.summonEntity` does
    /// this through `getLastEntity()`).
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn summon_entity(
        &mut self,
        fid: usize,
        target_cell: usize,
        chip_id: i32,
        name: Option<&str>,
    ) -> (i32, Option<usize>) {
        let Some(spec) = self.chip_specs.get(&chip_id).cloned() else {
            return (USE_INVALID_TARGET, None);
        };
        let Some(params) = spec
            .effects
            .iter()
            .find(|p| p.effect == EffectType::Summon)
            .cloned()
        else {
            return (USE_INVALID_TARGET, None);
        };
        if self.order.current() != Some(fid) {
            return (USE_INVALID_TARGET, None);
        }
        if spec.cost > self.fighters[fid].tp() {
            return (USE_NOT_ENOUGH_TP, None);
        }
        if self.has_chip_cooldown(fid, &spec) {
            return (USE_INVALID_COOLDOWN, None);
        }
        let Some(caster_cell) = self.fighters[fid].cell else {
            return (USE_INVALID_POSITION, None);
        };
        if !self.can_use_attack(
            caster_cell,
            target_cell,
            spec.min_range,
            spec.max_range,
            spec.launch_type,
            spec.needs_los,
            spec.area,
        ) {
            return (USE_INVALID_POSITION, None);
        }
        // `Cell.available(map)` — walkable and unoccupied.
        if !self.map.cells[target_cell].walkable || self.entity_on(target_cell).is_some() {
            return (USE_INVALID_POSITION, None);
        }
        let team = self.fighters[fid].team;
        if self.teams[team].summon_count(&self.fighters) >= SUMMON_LIMIT {
            return (USE_TOO_MANY_SUMMONS, None);
        }

        let critical = self.generate_critical(fid);
        let result = if critical { USE_CRITICAL } else { USE_SUCCESS };
        self.actions.log(Action::UseChip {
            chip_template: spec.id,
            cell: target_cell as i32,
            success: result,
        });
        // caster.onCritical(): passive effects — none in leek scope.

        let bulb = self.create_summon(
            fid,
            params.value1 as i32,
            target_cell,
            spec.level,
            critical,
            name,
        );

        // `ActionInvocation` carries FIDs (`getSummoner().getFId()` /
        // `target.getFId()`), not the bulb's negative real id.
        self.actions.log(Action::Invocation {
            owner_id: fid as i64,
            summon_id: bulb as i64,
            cell: target_cell as i32,
            result,
        });

        if spec.cooldown != 0 {
            self.add_chip_cooldown(fid, &spec, spec.cooldown);
        }
        self.fighters[fid].use_tp(spec.cost);
        (result, Some(bulb))
    }

    /// `State.createSummon(owner, type, target, level, critical, name)` +
    /// `Bulb.create` / `BulbTemplate.createInvocation` — build the bulb
    /// fighter (stats scaled by the *owner's* level, frequency 0, RAM 6
    /// capping the template chips), insert it into the team / arena / play
    /// order, place it, and append its `fight.leeks` entry. The birth turn is
    /// what `Fight.summonEntity` pins right after (`setBirthTurn(getTurn())`).
    /// Unknown templates panic — coverage is corpus-driven.
    #[allow(clippy::cast_possible_wrap)]
    pub fn create_summon(
        &mut self,
        owner: usize,
        template_id: i32,
        target_cell: usize,
        level: i32,
        critical: bool,
        name: Option<&str>,
    ) -> usize {
        let template = self
            .bulb_templates
            .get(&template_id)
            .unwrap_or_else(|| panic!("official port: bulb template {template_id} not registered"))
            .clone();

        // `BulbTemplate.createInvocation` — coeff from the owner's level,
        // 1.2x on critical.
        let coeff = f64::from(self.fighters[owner].level.min(300)) / 300.0;
        let multiplier = if critical { 1.2 } else { 1.0 };
        let mut stats = Stats::default();
        stats.set(STAT_LIFE, bulb_base(template.life, coeff, multiplier));
        stats.set(
            STAT_STRENGTH,
            bulb_base(template.strength, coeff, multiplier),
        );
        stats.set(STAT_WISDOM, bulb_base(template.wisdom, coeff, multiplier));
        stats.set(STAT_AGILITY, bulb_base(template.agility, coeff, multiplier));
        stats.set(
            STAT_RESISTANCE,
            bulb_base(template.resistance, coeff, multiplier),
        );
        stats.set(STAT_SCIENCE, bulb_base(template.science, coeff, multiplier));
        stats.set(STAT_MAGIC, bulb_base(template.magic, coeff, multiplier));
        stats.set(STAT_TP, bulb_base(template.tp, coeff, multiplier));
        stats.set(STAT_MP, bulb_base(template.mp, coeff, multiplier));
        stats.set(STAT_CORES, 1);
        stats.set(STAT_RAM, 6);
        // Frequency stays 0 (the Bulb constructor passes a literal 0).

        let fid = self.fighters.len(); // getNextEntityId()
        let bulb_name = match name {
            // `Bulb.create` truncates an override at 20 chars.
            Some(n) if !n.is_empty() => n.chars().take(20).collect(),
            _ => template.name.clone(),
        };
        let mut fighter = Fighter::new(fid, -(fid as i64), bulb_name, 0, stats);
        fighter.level = level;
        fighter.farmer = self.fighters[owner].farmer;
        fighter.ai_name.clone_from(&self.fighters[owner].ai_name);
        fighter.skin = template.id;
        fighter.summoner = Some(owner);
        fighter.birth_turn = self.order.turn();
        // `Entity.addChip` caps at the bulb's RAM (6).
        for &chip in &template.chips {
            if fighter.chips.len() < 6 {
                fighter.chips.insert(chip);
            }
        }

        let team = self.fighters[owner].team;
        let fid = self.add_entity(team, fighter);
        self.order.add_summon(owner, fid);
        self.place_entity(fid, target_cell);

        // `actions.addEntity(invoc, critical)` — appended to `fight.leeks`
        // at creation time.
        let snapshot = entity_snapshot(&self.fighters[fid], critical);
        self.leek_snapshots.push(snapshot);

        fid
    }

    // ── Resurrection ─────────────────────────────────────────────────────────

    /// `State.resurrectEntity(caster, target, template, target_entity,
    /// fullLife)` — the exact check ladder. Note the order quirk:
    /// `canUseAttack` (-4) comes **before** `hasCooldown` (-3), the reverse
    /// of `useChip`'s ladder. Like `summonEntity` there is no max-uses check
    /// and no `addItemUse`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn resurrect_entity(
        &mut self,
        fid: usize,
        target_cell: usize,
        chip_id: i32,
        target_entity: usize,
        full_life: bool,
    ) -> i32 {
        let Some(spec) = self.chip_specs.get(&chip_id).cloned() else {
            return USE_INVALID_TARGET;
        };
        if self.order.current() != Some(fid) {
            return USE_INVALID_TARGET;
        }
        if spec.cost > self.fighters[fid].tp() {
            return USE_NOT_ENOUGH_TP;
        }
        let Some(caster_cell) = self.fighters[fid].cell else {
            return USE_INVALID_POSITION;
        };
        if !self.can_use_attack(
            caster_cell,
            target_cell,
            spec.min_range,
            spec.max_range,
            spec.launch_type,
            spec.needs_los,
            spec.area,
        ) {
            return USE_INVALID_POSITION;
        }
        if self.has_chip_cooldown(fid, &spec) {
            return USE_INVALID_COOLDOWN;
        }
        // `params == null || !target.available(map) || !target_entity.isDead()`.
        if !spec
            .effects
            .iter()
            .any(|p| p.effect == EffectType::Resurrect)
            || !self.map.cells[target_cell].walkable
            || self.entity_on(target_cell).is_some()
            || !self.fighters[target_entity].is_dead()
        {
            return USE_INVALID_TARGET;
        }
        if self.fighters[target_entity].is_summon() {
            let team = self.fighters[target_entity].team;
            if self.teams[team].summon_count(&self.fighters) >= SUMMON_LIMIT {
                return USE_TOO_MANY_SUMMONS;
            }
        }

        let critical = self.generate_critical(fid);
        let result = if critical { USE_CRITICAL } else { USE_SUCCESS };
        self.actions.log(Action::UseChip {
            chip_template: spec.id,
            cell: target_cell as i32,
            success: result,
        });
        // caster.onCritical(): passive effects — none in leek scope.

        self.resurrect(fid, target_entity, target_cell, critical, full_life);

        if spec.cooldown != 0 {
            self.add_chip_cooldown(fid, &spec, spec.cooldown);
        }
        // Upstream hardcodes a 3-turn INVINCIBLE ADD_STATE when the chip is
        // 415 ("Awakening", the full-life variant) — only chip 84 is
        // registered in the corpus, so that branch stays unported.
        self.fighters[fid].use_tp(spec.cost);
        result
    }

    /// `State.resurrect(owner, entity, cell, critical, fullLife)` — re-insert
    /// the dead entity into the play order right before the first *alive*
    /// entity that followed it in the initial order (appending when none is
    /// left), restore its life (`Entity.resurrect`), put it back on the map
    /// and log `ActionResurrect`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    pub fn resurrect(
        &mut self,
        owner: usize,
        entity: usize,
        cell: usize,
        critical: bool,
        full_life: bool,
    ) {
        let mut next = None;
        let mut start = false;
        for &e in &self.initial_order {
            if e == entity {
                start = true;
                continue;
            }
            if !start || self.fighters[e].is_dead() {
                continue;
            }
            next = Some(e);
            break;
        }
        match next {
            None => self.order.add_entity(entity),
            Some(next) => {
                // `next` is alive, so it IS in the order: turn order ≥ 1.
                let index = usize::try_from(self.order.entity_turn_order(next) - 1)
                    .expect("alive entity has a turn order");
                self.order.insert_entity(index, entity);
            }
        }

        // `Entity.resurrect(owner, factor, fullLife)` — Effect.CRITICAL_FACTOR
        // is 1.3; the non-full-life revival halves the max life (floor 10)
        // and wakes up at half of that (integer division).
        let factor = if critical { 1.3 } else { 1.0 };
        let f = &mut self.fighters[entity];
        if full_life {
            f.life = f.total_life;
        } else {
            f.total_life = java_round(f64::from(f.total_life) * 0.5 * factor).max(10);
            f.life = f.total_life / 2;
        }
        // `endTurn()` — per-turn counters reset; the propagation sweep is
        // vacuous here (every effect on the entity was dropped on death).
        f.end_turn();

        self.place_entity(entity, cell);

        self.actions.log(Action::Resurrect {
            owner_id: owner as i64,
            target_id: entity as i64,
            cell: cell as i32,
            life: self.fighters[entity].life,
            max_life: self.fighters[entity].total_life,
        });
    }

    /// `EntityAI.addSystemLog` → `FarmerLog.addSystemLogString`: buffer a
    /// `[fid, type, trace, key, params?]` entry on the entity's farmer log,
    /// grouped under the id of the most recent action
    /// (`max(0, Actions.getNextId() - 1)`).
    ///
    /// The trace element is Java's rendering of its own AI call stack
    /// (`"\t▶ runIA, java line N\n"` — codegen line numbers); it can't be
    /// reproduced from Rust, so we emit `""` and the conformance diff
    /// normalizes the element away on both sides. The `FarmerLog` size
    /// budget (500k / `TOO_MUCH_DEBUG`) is out of scope — corpus logs are
    /// tiny.
    pub fn add_system_log(&mut self, fid: usize, log_type: i32, key: i32, params: Option<&[&str]>) {
        let action_key = self.actions.get_next_id().saturating_sub(1);
        let mut entry = vec![json!(fid), json!(log_type), json!(""), json!(key)];
        if let Some(params) = params {
            entry.push(json!(params));
        }
        let farmer = self.fighters[fid].farmer;
        self.farmer_logs
            .entry(farmer)
            .or_default()
            .entry(action_key)
            .or_default()
            .push(Value::Array(entry));
    }

    /// The end-of-fight invocation sweep (`Fight.java`: every summon is
    /// `removeInvocation`d from its team **before** `computeWinner` and
    /// `getDeadReport`), so summons never appear in the dead report or the
    /// winner computation.
    pub fn remove_all_invocations(&mut self) {
        for team in &mut self.teams {
            team.fighters.retain(|&f| !self.fighters[f].is_summon());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Fighter, Order, STAT_LIFE, STAT_TP, Stats, decrement_or_remove};
    use std::collections::BTreeMap;

    fn order_of(fids: &[usize]) -> Order {
        let mut o = Order::new();
        for &f in fids {
            o.add_entity(f);
        }
        o
    }

    /// Plain round-robin: next() wraps and bumps the turn exactly like Java.
    #[test]
    fn next_wraps_and_increments_turn() {
        let mut o = order_of(&[10, 20]);
        assert_eq!(o.current(), Some(10));
        assert_eq!(o.turn(), 1);
        assert!(!o.next());
        assert_eq!(o.current(), Some(20));
        assert!(o.next()); // wrap → turn 2
        assert_eq!(o.turn(), 2);
        assert_eq!(o.current(), Some(10));
    }

    /// Removing an entity *before* the current position shifts the position
    /// back so the current entity is unchanged.
    #[test]
    fn remove_before_position_keeps_current() {
        let mut o = order_of(&[1, 2, 3]);
        o.next(); // position 1 (entity 2)
        o.remove_entity(1);
        assert_eq!(o.current(), Some(2));
        assert_eq!(o.position(), 0);
    }

    /// Removing the current entity at position 0 rolls back to the end of
    /// the previous round (turn decremented; `next()` re-increments).
    #[test]
    fn remove_current_at_position_zero_rolls_back_turn() {
        let mut o = order_of(&[1, 2, 3]);
        assert_eq!(o.turn(), 1);
        o.remove_entity(1); // current, index 0
        // After removal: fids = [2, 3], position = -1 → len-1 = 1, turn 0.
        assert_eq!(o.position(), 1);
        assert_eq!(o.current(), Some(3));
        assert_eq!(o.turn(), 0);
        assert!(o.next()); // wraps → turn 1, position 0
        assert_eq!(o.turn(), 1);
        assert_eq!(o.current(), Some(2));
    }

    /// Removing an entity *after* the current position leaves it alone.
    #[test]
    fn remove_after_position_no_fixup() {
        let mut o = order_of(&[1, 2, 3]);
        o.remove_entity(3);
        assert_eq!(o.current(), Some(1));
        assert_eq!(o.position(), 0);
        assert!(!o.next());
        assert_eq!(o.current(), Some(2));
        assert!(o.next());
        assert_eq!(o.turn(), 2);
    }

    /// Summon insertion at or before the position shifts it (Java
    /// `addEntity(int, Entity)`).
    #[test]
    fn insert_at_position_shifts() {
        let mut o = order_of(&[1, 2]);
        o.next(); // position 1 (entity 2)
        o.insert_entity(1, 9); // at the position
        assert_eq!(o.current(), Some(2)); // unchanged
        assert_eq!(o.fids(), &[1, 9, 2]);
    }

    /// Cooldowns decrement by 1 and disappear at 0 (`decrementOrRemove`).
    #[test]
    fn cooldown_decrement_semantics() {
        let mut cds: BTreeMap<i32, i32> = BTreeMap::new();
        cds.insert(1, 1);
        cds.insert(2, 2);
        cds.insert(3, 5);
        decrement_or_remove(&mut cds);
        assert_eq!(cds.get(&1), None); // reached 0 → removed
        assert_eq!(cds.get(&2), Some(&1));
        assert_eq!(cds.get(&3), Some(&4));
    }

    /// TP/MP are total-minus-used; buffs raise the total live.
    #[test]
    fn tp_total_minus_used_with_buffs() {
        let mut stats = Stats::default();
        stats.set(STAT_LIFE, 500);
        stats.set(STAT_TP, 6);
        let mut f = Fighter::new(0, 1, "L".into(), 0, stats);
        assert_eq!(f.life, 500);
        assert_eq!(f.tp(), 6);
        f.use_tp(4);
        assert_eq!(f.tp(), 2);
        f.buff_stats.update(STAT_TP, 3);
        assert_eq!(f.tp(), 5);
    }
}
