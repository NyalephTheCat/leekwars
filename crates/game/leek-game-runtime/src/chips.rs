//! Chip catalog — real leek-wars stats (from the generator's
//! `data/chips.json`), keyed by the public `CHIP_*` item id.
//!
//! Effect *kinds* are assigned from the chip's known behavior (the JSON's
//! numeric `type` uses a different encoding than the engine's effect types),
//! with the real `value1`/`value2`/`turns`. Damage/heal are instant;
//! shields and buffs last `turns` turns.

use crate::{Effect, EffectKind, Stat};

/// One chip's stats.
#[derive(Debug, Clone, Copy)]
pub struct Chip {
    /// Public `CHIP_*` item id (e.g. `CHIP_SPARK` = 18).
    pub item: i64,
    pub name: &'static str,
    pub cost: i64,
    pub min_range: i64,
    pub max_range: i64,
    /// Area diameter (1 = single cell).
    pub area: i64,
    /// Turns before reuse (0 = none).
    pub cooldown: i64,
    /// Max uses per turn (0 = unlimited).
    pub max_uses: i64,
    pub effects: &'static [Effect],
}

use EffectKind::{AbsoluteShield, Damage, Heal};
const BUFF_STRENGTH: EffectKind = EffectKind::Buff(Stat::Strength);

static CATALOG: &[Chip] = &[
    // Damage.
    Chip {
        item: 18,
        name: "spark",
        cost: 3,
        min_range: 0,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 10,
        effects: &[Effect::new(Damage, 8, 8, 0)],
    },
    Chip {
        item: 6,
        name: "flash",
        cost: 3,
        min_range: 1,
        area: 1,
        cooldown: 1,
        max_uses: 0,
        max_range: 10,
        effects: &[Effect::new(Damage, 32, 3, 0)],
    },
    Chip {
        item: 5,
        name: "flame",
        cost: 4,
        min_range: 2,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 7,
        effects: &[Effect::new(Damage, 29, 2, 0)],
    },
    Chip {
        item: 33,
        name: "lightning",
        cost: 4,
        min_range: 2,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 5,
        effects: &[Effect::new(Damage, 35, 12, 0)],
    },
    // Heal (wisdom-scaled).
    Chip {
        item: 3,
        name: "bandage",
        cost: 2,
        min_range: 0,
        area: 1,
        cooldown: 1,
        max_uses: 0,
        max_range: 6,
        effects: &[Effect::new(Heal, 13, 5, 0)],
    },
    Chip {
        item: 4,
        name: "cure",
        cost: 4,
        min_range: 0,
        area: 1,
        cooldown: 2,
        max_uses: 0,
        max_range: 5,
        effects: &[Effect::new(Heal, 35, 8, 0)],
    },
    Chip {
        item: 11,
        name: "vaccine",
        cost: 6,
        min_range: 0,
        area: 1,
        cooldown: 3,
        max_uses: 0,
        max_range: 6,
        effects: &[Effect::new(Heal, 38, 4, 0)],
    },
    // Absolute shield (resistance-scaled), lasting a few turns.
    Chip {
        item: 21,
        name: "helmet",
        cost: 3,
        min_range: 0,
        area: 1,
        cooldown: 3,
        max_uses: 0,
        max_range: 4,
        effects: &[Effect::new(AbsoluteShield, 15, 0, 2)],
    },
    Chip {
        item: 20,
        name: "shield",
        cost: 4,
        min_range: 0,
        area: 1,
        cooldown: 4,
        max_uses: 0,
        max_range: 4,
        effects: &[Effect::new(AbsoluteShield, 20, 0, 3)],
    },
    Chip {
        item: 22,
        name: "armor",
        cost: 6,
        min_range: 0,
        area: 1,
        cooldown: 5,
        max_uses: 0,
        max_range: 4,
        effects: &[Effect::new(AbsoluteShield, 25, 0, 4)],
    },
    // Strength buff (science-scaled).
    Chip {
        item: 8,
        name: "protein",
        cost: 3,
        min_range: 0,
        area: 1,
        cooldown: 3,
        max_uses: 0,
        max_range: 4,
        effects: &[Effect::new(BUFF_STRENGTH, 80, 20, 2)],
    },
    // Poison (magic-scaled, per-turn).
    Chip {
        item: 97,
        name: "venom",
        cost: 4,
        min_range: 1,
        area: 1,
        cooldown: 1,
        max_uses: 0,
        max_range: 10,
        effects: &[Effect::new(EffectKind::Poison, 15, 5, 3)],
    },
    // Regeneration (wisdom-scaled, per-turn heal).
    Chip {
        item: 35,
        name: "regeneration",
        cost: 8,
        min_range: 0,
        area: 1,
        cooldown: 99,
        max_uses: 0,
        max_range: 3,
        effects: &[Effect::new(EffectKind::Regeneration, 50, 0, 3)],
    },
    // Relative vulnerability (negative shield).
    Chip {
        item: 106,
        name: "fracture",
        cost: 4,
        min_range: 1,
        area: 1,
        cooldown: 1,
        max_uses: 0,
        max_range: 6,
        effects: &[Effect::new(
            EffectKind::Vulnerability { absolute: false },
            20,
            5,
            2,
        )],
    },
    // Strength shackle (magic-scaled debuff).
    Chip {
        item: 94,
        name: "tranquilizer",
        cost: 3,
        min_range: 1,
        area: 1,
        cooldown: 0,
        max_uses: 0,
        max_range: 8,
        effects: &[Effect::new(EffectKind::Shackle(Stat::Strength), 20, 5, 2)],
    },
    // Cure poison.
    Chip {
        item: 110,
        name: "antidote",
        cost: 3,
        min_range: 0,
        area: 1,
        cooldown: 4,
        max_uses: 0,
        max_range: 4,
        effects: &[Effect::new(EffectKind::Antidote, 0, 0, 0)],
    },
    // Revive a dead ally.
    Chip {
        item: 84,
        name: "resurrection",
        cost: 15,
        min_range: 1,
        area: 1,
        cooldown: 15,
        max_uses: 0,
        max_range: 2,
        effects: &[Effect::new(EffectKind::Resurrect, 100, 0, 0)],
    },
];

/// Look up a chip by its public item id.
#[must_use]
pub fn lookup(item: i64) -> Option<&'static Chip> {
    CATALOG.iter().find(|c| c.item == item)
}
