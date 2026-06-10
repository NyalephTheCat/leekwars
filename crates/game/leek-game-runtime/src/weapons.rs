//! Weapon catalog — real leek-wars stats (from the generator's
//! `data/weapons.json`), keyed by the public `WEAPON_*` item id.
//!
//! Only the **direct-damage** effects (effect `type: 1`) are modeled here, as
//! `(value1, value2)` pairs — base damage rolls `value1 + jet·value2`. Other
//! effects (poison, shield-steal, buffs) and area/line-of-sight are not yet
//! applied; see [`crate::call_game_builtin`].

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
    /// Area diameter (1 = single cell; N hits cells within Manhattan radius
    /// `N-1` of the target).
    pub area: i64,
    /// Turns before reuse (0 = none).
    pub cooldown: i64,
    /// Max uses per turn (0 = unlimited).
    pub max_uses: i64,
    /// Direct-damage effects as `(value1, value2)`: each rolls
    /// `value1 + jet·value2` damage. Multiple entries = multi-hit (e.g. the
    /// machine gun's three bullets).
    pub damages: &'static [(i64, i64)],
}

/// The modeled weapons (the common direct-damage set).
static CATALOG: &[Weapon] = &[
    Weapon {
        item: 37,
        name: "pistol",
        cost: 3,
        min_range: 1,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 7,
        damages: &[(15, 5)],
    },
    Weapon {
        item: 38,
        name: "machine_gun",
        cost: 4,
        min_range: 1,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 6,
        damages: &[(10, 5), (10, 5), (10, 5)],
    },
    Weapon {
        item: 39,
        name: "double_gun",
        cost: 4,
        min_range: 2,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 7,
        damages: &[(18, 7)],
    },
    Weapon {
        item: 40,
        name: "destroyer",
        cost: 6,
        min_range: 1,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 6,
        damages: &[(40, 20)],
    },
    Weapon {
        item: 41,
        name: "shotgun",
        cost: 5,
        min_range: 1,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 5,
        damages: &[(33, 10)],
    },
    Weapon {
        item: 42,
        name: "laser",
        cost: 6,
        min_range: 2,
        area: 2,
        cooldown: 0,
        max_uses: 0,
        max_range: 9,
        damages: &[(43, 16)],
    },
    Weapon {
        item: 43,
        name: "grenade_launcher",
        cost: 6,
        min_range: 4,
        area: 4,
        cooldown: 0,
        max_uses: 0,
        max_range: 7,
        damages: &[(45, 8)],
    },
    Weapon {
        item: 45,
        name: "magnum",
        cost: 5,
        min_range: 1,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 8,
        damages: &[(25, 15)],
    },
    Weapon {
        item: 47,
        name: "m_laser",
        cost: 8,
        min_range: 5,
        area: 2,
        cooldown: 0,
        max_uses: 0,
        max_range: 12,
        damages: &[(90, 10)],
    },
    Weapon {
        item: 151,
        name: "rifle",
        cost: 7,
        min_range: 7,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 9,
        damages: &[(73, 6)],
    },
];

/// Look up a weapon by its public item id.
#[must_use]
pub fn lookup(item: i64) -> Option<&'static Weapon> {
    CATALOG.iter().find(|w| w.item == item)
}
