//! The reference world model: [`Fight`] — entities on a `width × height`
//! grid — and its [`GameHost`] implementation, plus the shared [`FightRef`]
//! handle orchestrators drive it through.
//!
//! - **Map**: a square grid, cells numbered row-major. Geometry uses grid
//!   (Manhattan) distance for movement and Euclidean for `getDistance`. The
//!   real leek-wars diamond grid can be substituted by changing the
//!   coordinate methods only.
//! - **Actions**: movement consumes MP and walks the grid greedily; combat
//!   spends TP and applies the catalog damage formulas.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::effect::{MODIFIER_IRREDUCTIBLE, MODIFIER_STACKABLE};
use crate::{ActiveEffect, EffectKind, Entity, GameHost, Stat};

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
    /// `(item, effect_id)` pairs already warned about, so each unsupported
    /// effect surfaces in the log once per fight.
    warned: HashSet<(i64, u8)>,
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
            warned: HashSet::new(),
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

    /// Set the current turn number (the orchestrator's turn loop advances it).
    pub fn set_turn(&mut self, turn: i64) {
        self.turn = turn;
    }

    /// Reset an entity's MP/TP to their (buff-adjusted) maxima
    /// (start-of-turn regen).
    pub fn regen(&mut self, entity: i64) {
        if let Some(e) = self.entity_mut(entity) {
            e.mp = (e.max_mp + e.buff_sum(Stat::Mp)).max(0);
            e.tp = (e.max_tp + e.buff_sum(Stat::Tp)).max(0);
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
        self.get(entity)
            .map(|e| e.strength + e.buff_sum(Stat::Strength))
    }
    fn wisdom(&self, entity: i64) -> Option<i64> {
        self.get(entity)
            .map(|e| e.wisdom + e.buff_sum(Stat::Wisdom))
    }
    fn agility(&self, entity: i64) -> Option<i64> {
        self.get(entity)
            .map(|e| e.agility + e.buff_sum(Stat::Agility))
    }
    fn resistance(&self, entity: i64) -> Option<i64> {
        self.get(entity)
            .map(|e| e.resistance + e.buff_sum(Stat::Resistance))
    }
    fn science(&self, entity: i64) -> Option<i64> {
        self.get(entity)
            .map(|e| e.science + e.buff_sum(Stat::Science))
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
        self.get(entity)
            .map(|e| e.damage_return + e.effect_sum(EffectKind::DamageReturn))
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
        self.get(entity)
            .map(|e| e.inventory.clone())
            .unwrap_or_default()
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
        self.get(entity).map_or(0, |e| {
            e.relative_shield + e.effect_sum(EffectKind::RelativeShield)
        })
    }
    fn absolute_shield(&self, entity: i64) -> i64 {
        self.get(entity).map_or(0, |e| {
            e.absolute_shield + e.effect_sum(EffectKind::AbsoluteShield)
        })
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

    fn add_effect(&mut self, entity: i64, effect: ActiveEffect) {
        let Some(e) = self.entity_mut(entity) else {
            return;
        };
        // MP/TP pool delta accumulated across replace/add, applied once.
        let mut d_mp = 0;
        let mut d_tp = 0;
        // Non-stackable lasting effects replace the previous effect with the
        // same (kind, item) — the first match, as upstream `createEffect`.
        if effect.turns != 0
            && effect.modifiers & MODIFIER_STACKABLE == 0
            && let Some(i) = e
                .effects
                .iter()
                .position(|x| x.kind == effect.kind && x.item == effect.item)
        {
            let old = e.effects.remove(i);
            match old.kind {
                EffectKind::Buff(Stat::Mp) => d_mp -= old.value,
                EffectKind::Buff(Stat::Tp) => d_tp -= old.value,
                _ => {}
            }
        }
        // Zero-value effects are dropped (upstream only stores `value > 0`).
        if effect.turns != 0 && effect.value != 0 {
            // MP/TP buffs grant (or shackles remove) the points immediately;
            // regen() folds the buff totals back in each turn.
            match effect.kind {
                EffectKind::Buff(Stat::Mp) => d_mp += effect.value,
                EffectKind::Buff(Stat::Tp) => d_tp += effect.value,
                _ => {}
            }
            // A cast with the same identity merges into the existing entry
            // instead of adding a second one.
            if let Some(x) = e.effects.iter_mut().find(|x| {
                x.kind == effect.kind
                    && x.item == effect.item
                    && x.turns == effect.turns
                    && x.caster == effect.caster
            }) {
                x.value += effect.value;
            } else {
                e.effects.push(effect);
            }
        }
        e.mp = (e.mp + d_mp).max(0);
        e.tp = (e.tp + d_tp).max(0);
    }

    fn grant_vitality(&mut self, entity: i64, amount: i64) {
        if let Some(e) = self.entity_mut(entity) {
            let amount = amount.max(0);
            e.max_life += amount;
            e.life += amount;
        }
    }

    fn raise_max_life(&mut self, entity: i64, amount: i64) {
        if let Some(e) = self.entity_mut(entity) {
            e.max_life += amount.max(0);
        }
    }

    fn remove_effects(&mut self, entity: i64, kind: EffectKind) {
        if let Some(e) = self.entity_mut(entity) {
            e.effects.retain(|eff| eff.kind != kind);
        }
    }

    fn reduce_effects(&mut self, entity: i64, percent: f64, total: bool) {
        let Some(e) = self.entity_mut(entity) else {
            return;
        };
        let reduction = (1.0 - percent).max(0.0);
        // MP/TP buff values changing mid-turn adjust the pools by the delta.
        let (mut d_mp, mut d_tp) = (0, 0);
        e.effects.retain_mut(|eff| {
            // Irreductible effects survive a normal debuff (but not a
            // TOTAL_DEBUFF).
            if !total && eff.modifiers & MODIFIER_IRREDUCTIBLE != 0 {
                return true;
            }
            // Sign-safe rounding (Java rounds the magnitude, keeps the sign).
            #[allow(clippy::cast_possible_truncation)]
            let new = (f64::from(i32::try_from(eff.value.abs()).unwrap_or(0)) * reduction).round()
                as i64
                * eff.value.signum();
            match eff.kind {
                EffectKind::Buff(Stat::Mp) => d_mp += new - eff.value,
                EffectKind::Buff(Stat::Tp) => d_tp += new - eff.value,
                _ => {}
            }
            eff.value = new;
            new != 0
        });
        e.mp = (e.mp + d_mp).max(0);
        e.tp = (e.tp + d_tp).max(0);
    }

    fn has_item_effect(&self, entity: i64, item: i64) -> bool {
        self.get(entity)
            .is_some_and(|e| e.effects.iter().any(|eff| eff.item == item))
    }

    fn warn_unsupported(&mut self, entity: i64, item: i64, effect_id: u8) {
        if self.warned.insert((item, effect_id)) {
            self.log.push((
                entity,
                format!("[engine] item {item}: effect type {effect_id} not modeled — skipped"),
            ));
        }
    }

    fn remove_shackles(&mut self, entity: i64) {
        let Some(e) = self.entity_mut(entity) else {
            return;
        };
        // Shackles are stored as negative stat buffs. Removing an MP/TP
        // shackle gives the points back immediately.
        let (mut d_mp, mut d_tp) = (0, 0);
        e.effects.retain(|eff| {
            let shackle = matches!(eff.kind, EffectKind::Buff(_)) && eff.value < 0;
            if shackle {
                match eff.kind {
                    EffectKind::Buff(Stat::Mp) => d_mp -= eff.value,
                    EffectKind::Buff(Stat::Tp) => d_tp -= eff.value,
                    _ => {}
                }
            }
            !shackle
        });
        e.mp = (e.mp + d_mp).max(0);
        e.tp = (e.tp + d_tp).max(0);
    }

    fn revive(&mut self, entity: i64, life: i64) {
        if let Some(e) = self.entity_mut(entity)
            && e.life <= 0
        {
            e.life = life.clamp(1, e.max_life);
        }
    }

    fn cooldown(&self, entity: i64, item: i64) -> i64 {
        self.get(entity)
            .and_then(|e| e.item_cooldowns.get(&item).copied())
            .unwrap_or(0)
    }
    fn uses_this_turn(&self, entity: i64, item: i64) -> i64 {
        self.get(entity)
            .and_then(|e| e.item_uses.get(&item).copied())
            .unwrap_or(0)
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
