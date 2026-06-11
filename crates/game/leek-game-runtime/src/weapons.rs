//! Weapon catalog — real leek-wars stats, generated from the official
//! generator's `data/weapons.json` by `tools/game-item-extract.sh --write`
//! and keyed by the public `WEAPON_*` item id.
//!
//! Each weapon carries its full upstream effect list (damage, poison,
//! shackles, …); effect-type ids the engine doesn't model yet come through as
//! [`EffectKind::Unsupported`](crate::EffectKind) and are skipped when fired.

use crate::effect::Effect;

/// One weapon's combat stats.
#[derive(Debug, Clone, Copy)]
pub struct Weapon {
    /// Public `WEAPON_*` item id (e.g. `WEAPON_PISTOL` = 37).
    pub item: i64,
    pub name: &'static str,
    /// TP cost per use.
    pub cost: i64,
    pub min_range: i64,
    pub max_range: i64,
    /// Upstream launch-type id (line, diagonal, …). Carried, not yet honored
    /// (any in-range cell can be targeted).
    pub launch_type: i64,
    /// Area diameter (1 = single cell; N hits cells within Manhattan radius
    /// `N-1` of the target).
    pub area: i64,
    /// Whether use requires line of sight to the target.
    pub los: bool,
    /// Turns before reuse (0 = none).
    pub cooldown: i64,
    /// Max uses per turn (0 = unlimited).
    pub max_uses: i64,
    /// The weapon's effects, applied in order on each use. Multiple `Damage`
    /// entries = multi-hit (e.g. the machine gun's three bullets).
    pub effects: &'static [Effect],
}

include!("weapons_gen.rs");

/// Look up a weapon by its public item id.
#[must_use]
pub fn lookup(item: i64) -> Option<&'static Weapon> {
    CATALOG.iter().find(|w| w.item == item)
}

/// The upstream effect-type ids of this weapon's effects the engine doesn't
/// model (empty = fully supported), or `None` if the weapon isn't in the
/// catalog at all. Used by strict-mode scenario validation.
#[must_use]
pub fn unsupported_effects(item: i64) -> Option<Vec<u8>> {
    lookup(item).map(|w| crate::effect::unsupported_ids(w.effects))
}
