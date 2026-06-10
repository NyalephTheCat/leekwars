//! The [`GameHost`] seam the game builtins operate through.

use crate::EffectKind;

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
