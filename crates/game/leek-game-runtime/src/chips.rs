//! Chip catalog — real leek-wars stats, read from the vendored
//! `data/chips.json` (see [`crate::catalog`]) and keyed by the public
//! `CHIP_*` item id (the JSON key).
//!
//! Each chip carries its full upstream effect list; effect-type ids the
//! engine doesn't model yet come through as
//! [`EffectKind::Unsupported`](crate::EffectKind) and are skipped when cast.

use std::sync::OnceLock;

use crate::catalog;
use crate::effect::Effect;

/// One chip's stats.
#[derive(Debug, Clone)]
pub struct Chip {
    /// Public `CHIP_*` item id (e.g. `CHIP_SPARK` = 18).
    pub item: i64,
    pub name: String,
    pub cost: i64,
    pub min_range: i64,
    pub max_range: i64,
    /// Upstream launch-type bit mask (1 = line, 2 = diagonal, 4 = anything
    /// else). Carried, not yet honored.
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
    pub effects: Vec<Effect>,
}

static CATALOG: OnceLock<Vec<Chip>> = OnceLock::new();

/// The whole catalog, parsed on first use and ordered by item id.
fn catalog() -> &'static [Chip] {
    CATALOG.get_or_init(|| {
        catalog::rows(catalog::CHIPS_JSON, "chips")
            .iter()
            .map(|c| Chip {
                item: catalog::int(c, "id", 0),
                name: catalog::text(c, "name"),
                cost: catalog::int(c, "cost", 0),
                min_range: catalog::int(c, "min_range", 0),
                max_range: catalog::int(c, "max_range", 0),
                launch_type: catalog::int(c, "launch_type", 0),
                area: catalog::int(c, "area", 1),
                los: catalog::flag(c, "los", true),
                cooldown: catalog::int(c, "cooldown", 0),
                initial_cooldown: catalog::int(c, "initial_cooldown", 0),
                team_cooldown: catalog::flag(c, "team_cooldown", false),
                max_uses: catalog::int(c, "max_uses", 0),
                effects: crate::effect::parse_effects(catalog::entries(c, "effects")),
            })
            .collect()
    })
}

/// Look up a chip by its public item id.
#[must_use]
pub fn lookup(item: i64) -> Option<&'static Chip> {
    catalog().iter().find(|c| c.item == item)
}

/// The upstream effect-type ids of this chip's effects the engine doesn't
/// model (empty = fully supported), or `None` if the chip isn't in the
/// catalog at all. Used by strict-mode scenario validation.
#[must_use]
pub fn unsupported_effects(item: i64) -> Option<Vec<u8>> {
    lookup(item).map(|c| crate::effect::unsupported_ids(&c.effects))
}
