//! One combatant in the fight: [`Entity`], its stats, inventory, and active
//! effects, with `with_*` builders for test/scenario setup.

use std::collections::HashMap;

use crate::{ActiveEffect, EffectKind, Stat};

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
    pub fn with_magic_stats(
        mut self,
        wisdom: i64,
        resistance: i64,
        science: i64,
        magic: i64,
    ) -> Self {
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
    pub(crate) fn effect_sum(&self, kind: EffectKind) -> i64 {
        self.effects
            .iter()
            .filter(|e| e.kind == kind)
            .map(|e| e.value)
            .sum()
    }

    /// Sum active buffs to `stat` (added to the base value by the getters).
    pub(crate) fn buff_sum(&self, stat: Stat) -> i64 {
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
