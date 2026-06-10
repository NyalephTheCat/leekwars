//! Chip catalog — real leek-wars stats, generated from the official
//! generator's `data/chips.json` by `tools/game-item-extract.sh --write` and
//! keyed by the public `CHIP_*` item id (the JSON key).
//!
//! Each chip carries its full upstream effect list; effect-type ids the
//! engine doesn't model yet come through as
//! [`EffectKind::Unsupported`](crate::EffectKind) and are skipped when cast.

use crate::effect::Effect;

/// One chip's stats.
#[derive(Debug, Clone, Copy)]
pub struct Chip {
    /// Public `CHIP_*` item id (e.g. `CHIP_SPARK` = 18).
    pub item: i64,
    pub name: &'static str,
    pub cost: i64,
    pub min_range: i64,
    pub max_range: i64,
    /// Upstream launch-type id (line, diagonal, …). Carried, not yet honored.
    pub launch_type: i64,
    /// Area diameter (1 = single cell).
    pub area: i64,
    /// Whether use requires line of sight to the target.
    pub los: bool,
    /// Turns before reuse (0 = none, -1 = once per fight).
    pub cooldown: i64,
    /// Cooldown already running at fight start. Carried, not yet honored.
    pub initial_cooldown: i64,
    /// Whether the cooldown is shared by the whole team. Carried, not yet
    /// honored (cooldowns are tracked per entity).
    pub team_cooldown: bool,
    /// Max uses per turn (0 = unlimited).
    pub max_uses: i64,
    pub effects: &'static [Effect],
}

include!("chips_gen.rs");

/// Look up a chip by its public item id.
#[must_use]
pub fn lookup(item: i64) -> Option<&'static Chip> {
    CATALOG.iter().find(|c| c.item == item)
}

/// The upstream effect-type ids of this chip's effects the engine doesn't
/// model (empty = fully supported), or `None` if the chip isn't in the
/// catalog at all. Used by strict-mode scenario validation.
#[must_use]
pub fn unsupported_effects(item: i64) -> Option<Vec<u8>> {
    lookup(item).map(|c| crate::effect::unsupported_ids(c.effects))
}
