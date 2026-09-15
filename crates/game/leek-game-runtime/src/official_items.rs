//! Official item catalogs — the weapon, chip and bulb *templates* the
//! reference generator registers at startup (`Generator.loadWeapons` /
//! `loadChips` / `loadSummons`), generated from its
//! `data/{weapons,chips,summons}.json` by `tools/game-item-extract.sh --write`
//! into [`official_items_gen.rs`](../src/official_items_gen.rs).
//!
//! This is the official-model counterpart of [`crate::weapons`] /
//! [`crate::chips`]: those feed the legacy [`Fight`](crate::Fight) world with
//! `&'static` [`Weapon`](crate::weapons::Weapon) rows, while the official
//! [`State`](crate::state::State) wants owned
//! [`WeaponSpec`](crate::state::WeaponSpec) /
//! [`ChipSpec`](crate::state::ChipSpec) /
//! [`BulbTemplate`](crate::state::BulbTemplate) values in its template maps.
//!
//! The tables are raw rows rather than spec literals because the spec types
//! own their effect lines in a `Vec`, which no `static` can hold: a row keeps
//! the JSON's own fields and [`weapon_spec`] / [`chip_spec`] /
//! [`bulb_template`] build the real type on demand.

use crate::attack::{Area, EffectModifiers, EffectParams, EffectTargets, EffectType};
use crate::state::{BulbTemplate, ChipSpec, WeaponSpec};

// ─────────────────────────────────────────────────────────────────────────────
// Raw table rows
// ─────────────────────────────────────────────────────────────────────────────

/// One raw effect line of an item, exactly as `data/*.json` spells it.
#[derive(Debug, Clone, Copy)]
pub struct RawEffect {
    /// The generator's effect-type id (`Effect.TYPE_*` — the entry's `id`
    /// field, *not* its `type` field).
    pub id: i32,
    pub value1: f64,
    pub value2: f64,
    pub turns: i32,
    /// `Effect.TARGET_*` bit mask.
    pub targets: i32,
    /// `Effect.MODIFIER_*` bit mask.
    pub modifiers: i32,
}

/// One raw weapon row (`data/weapons.json`).
#[derive(Debug, Clone, Copy)]
pub struct RawWeapon {
    /// The public `WEAPON_*` item id (the entry's `item` field).
    pub item: i32,
    pub name: &'static str,
    pub cost: i32,
    pub min_range: i32,
    pub max_range: i32,
    pub launch_type: i32,
    /// `Area.TYPE_*` id.
    pub area: i32,
    pub los: bool,
    /// `Attack.getMaxUses()` — `-1` is unlimited.
    pub max_uses: i32,
    pub forgotten: bool,
    pub effects: &'static [RawEffect],
}

/// One raw chip row (`data/chips.json`).
#[derive(Debug, Clone, Copy)]
pub struct RawChip {
    /// The public `CHIP_*` item id (the JSON key).
    pub item: i32,
    pub name: &'static str,
    pub cost: i32,
    pub min_range: i32,
    pub max_range: i32,
    pub launch_type: i32,
    /// `Area.TYPE_*` id.
    pub area: i32,
    pub los: bool,
    /// `Attack.getMaxUses()` — `-1` is unlimited.
    pub max_uses: i32,
    /// `Chip.getCooldown()` — `0` is none, `-1` is "rest of the fight".
    pub cooldown: i32,
    pub initial_cooldown: i32,
    pub team_cooldown: bool,
    /// `Chip.getLevel()` — the level copied onto bulbs this chip summons.
    pub level: i32,
    pub effects: &'static [RawEffect],
}

/// One raw bulb row (`data/summons.json`) — `(min, max)` stat ranges plus the
/// granted chip template ids.
#[derive(Debug, Clone, Copy)]
pub struct RawBulb {
    pub id: i32,
    pub name: &'static str,
    pub life: (i32, i32),
    pub strength: (i32, i32),
    pub wisdom: (i32, i32),
    pub agility: (i32, i32),
    pub resistance: (i32, i32),
    pub science: (i32, i32),
    pub magic: (i32, i32),
    pub tp: (i32, i32),
    pub mp: (i32, i32),
    pub chips: &'static [i32],
}

include!("official_items_gen.rs");

// ─────────────────────────────────────────────────────────────────────────────
// Raw row → spec
// ─────────────────────────────────────────────────────────────────────────────

/// The row's effect lines as [`EffectParams`].
///
/// Panics on an effect-type id outside `Effect.TYPE_*`: the tables are
/// generated from the reference data, so an unknown id means the upstream
/// snapshot grew a type [`EffectType`] doesn't name yet, and silently
/// dropping the line would make the item quietly weaker than the real one.
fn effect_params(item: i32, effects: &[RawEffect]) -> Vec<EffectParams> {
    effects
        .iter()
        .map(|e| EffectParams {
            effect: EffectType::from_id(e.id)
                .unwrap_or_else(|| panic!("item {item}: unknown effect type id {}", e.id)),
            value1: e.value1,
            value2: e.value2,
            turns: e.turns,
            // Every mask in the generated tables is inside the known bits —
            // asserted by the `official_masks_are_known_bits` test, so the
            // truncation never actually drops one.
            targets: EffectTargets::from_bits_truncate(e.targets),
            modifiers: EffectModifiers::from_bits_truncate(e.modifiers),
        })
        .collect()
}

/// The area id as an [`Area`], panicking on an id outside `Area.TYPE_*`.
fn area_of(item: i32, area: i32) -> Area {
    Area::from_id(area).unwrap_or_else(|| panic!("item {item}: unknown area id {area}"))
}

/// The official [`WeaponSpec`] for a public `WEAPON_*` item id, or `None`
/// when the catalog has no such weapon.
#[must_use]
pub fn weapon_spec(id: i32) -> Option<WeaponSpec> {
    let w = OFFICIAL_WEAPONS.iter().find(|w| w.item == id)?;
    Some(WeaponSpec {
        id: w.item,
        cost: w.cost,
        min_range: w.min_range,
        max_range: w.max_range,
        launch_type: w.launch_type,
        needs_los: w.los,
        max_uses: w.max_uses,
        area: area_of(w.item, w.area),
        effects: effect_params(w.item, w.effects),
        forgotten: w.forgotten,
    })
}

/// The official [`ChipSpec`] for a public `CHIP_*` item id, or `None` when
/// the catalog has no such chip.
#[must_use]
pub fn chip_spec(id: i32) -> Option<ChipSpec> {
    let c = OFFICIAL_CHIPS.iter().find(|c| c.item == id)?;
    Some(ChipSpec {
        id: c.item,
        cost: c.cost,
        min_range: c.min_range,
        max_range: c.max_range,
        launch_type: c.launch_type,
        needs_los: c.los,
        max_uses: c.max_uses,
        area: area_of(c.item, c.area),
        effects: effect_params(c.item, c.effects),
        cooldown: c.cooldown,
        team_cooldown: c.team_cooldown,
        initial_cooldown: c.initial_cooldown,
        level: c.level,
    })
}

/// The official [`BulbTemplate`] for a summons template id, or `None` when
/// the catalog has no such bulb.
#[must_use]
pub fn bulb_template(id: i32) -> Option<BulbTemplate> {
    let b = OFFICIAL_BULBS.iter().find(|b| b.id == id)?;
    Some(BulbTemplate {
        id: b.id,
        name: b.name.to_string(),
        life: b.life,
        strength: b.strength,
        wisdom: b.wisdom,
        agility: b.agility,
        resistance: b.resistance,
        science: b.science,
        magic: b.magic,
        tp: b.tp,
        mp: b.mp,
        chips: b.chips.to_vec(),
    })
}

/// Every public weapon item id in the catalog, ascending.
pub fn weapon_ids() -> impl Iterator<Item = i32> {
    OFFICIAL_WEAPONS.iter().map(|w| w.item)
}

/// Every public chip item id in the catalog, ascending.
pub fn chip_ids() -> impl Iterator<Item = i32> {
    OFFICIAL_CHIPS.iter().map(|c| c.item)
}

/// Every bulb template id in the catalog, ascending.
pub fn bulb_ids() -> impl Iterator<Item = i32> {
    OFFICIAL_BULBS.iter().map(|b| b.id)
}

// ─────────────────────────────────────────────────────────────────────────────
// Effect coverage
// ─────────────────────────────────────────────────────────────────────────────

/// Whether the official attack path models this effect type.
///
/// Coverage grows corpus-first, so this is the inverse list: everything the
/// dispatch sites handle is ported, and these eight still hit
/// `State::create_effect`'s `not ported yet` panic arm. Keep it in step with
/// that `match` (plus the types intercepted before it:
/// `EffectType::Teleport` / `Propagation` in `State::apply_on_cell`,
/// `Summon` in `State::summon_entity` and `Resurrect` in `State::resurrect`).
fn is_ported(effect: EffectType) -> bool {
    !matches!(
        effect,
        EffectType::PoisonToScience
            | EffectType::DamageToAbsoluteShield
            | EffectType::DamageToStrength
            | EffectType::NovaDamageToMagic
            | EffectType::MovedToMp
            | EffectType::KillToTp
            | EffectType::CriticalToHeal
            | EffectType::DamageToResistance
    )
}

/// The effect types of this weapon the official attack path doesn't model
/// (empty = fully supported), or `None` when the catalog has no such weapon.
/// In item order, duplicates kept — the official-catalog counterpart of
/// [`crate::weapons::unsupported_effects`], for strict-mode validation.
#[must_use]
pub fn weapon_unsupported_effect_types(id: i32) -> Option<Vec<EffectType>> {
    let w = OFFICIAL_WEAPONS.iter().find(|w| w.item == id)?;
    Some(unsupported_of(w.item, w.effects))
}

/// The effect types of this chip the official attack path doesn't model
/// (empty = fully supported), or `None` when the catalog has no such chip.
/// In item order, duplicates kept — the official-catalog counterpart of
/// [`crate::chips::unsupported_effects`], for strict-mode validation.
#[must_use]
pub fn chip_unsupported_effect_types(id: i32) -> Option<Vec<EffectType>> {
    let c = OFFICIAL_CHIPS.iter().find(|c| c.item == id)?;
    Some(unsupported_of(c.item, c.effects))
}

fn unsupported_of(item: i32, effects: &[RawEffect]) -> Vec<EffectType> {
    effects
        .iter()
        .map(|e| {
            EffectType::from_id(e.id)
                .unwrap_or_else(|| panic!("item {item}: unknown effect type id {}", e.id))
        })
        .filter(|&t| !is_ported(t))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `WEAPON_PISTOL`.
    const PISTOL: i32 = 37;

    /// The generated pistol still carries the hand-written harness pistol's
    /// geometry and damage line (`harness_pistol` in `leek-scenario`'s
    /// `official-fight` bin — the `Harness.registerPistol` port).
    ///
    /// Two fields deliberately differ, and are asserted separately below:
    /// the harness registers a *synthetic* pistol whose `launchType` is the
    /// pre-2.50 legacy encoding and whose `maxUses` is unlimited, so the
    /// oracle goldens it produced stay valid as the real catalog moves.
    #[test]
    fn the_official_pistol_matches_the_hand_written_harness_spec() {
        let spec = weapon_spec(PISTOL).expect("the pistol is in the official catalog");
        assert_eq!(spec.id, 37);
        assert_eq!(spec.cost, 3);
        assert_eq!(spec.min_range, 1);
        assert_eq!(spec.max_range, 7);
        assert!(spec.needs_los);
        assert_eq!(spec.area, Area::SingleCell);
        assert!(!spec.forgotten);
        assert_eq!(
            spec.effects,
            vec![EffectParams {
                effect: EffectType::Damage,
                value1: 15.0,
                value2: 5.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            }]
        );
    }

    /// `launch_type` is a bitmask — 1 = line, 2 = diagonal, 4 = anything
    /// else (`Map::verify_range`) — and the upstream data now ships it that
    /// way instead of the legacy `0 = line / 1 = circle` pair. The pistol
    /// fires in every direction, so its mask is the full `1 | 2 | 4`; the
    /// value the harness fixture keeps (`1`) would restrict it to a line.
    #[test]
    fn the_official_pistol_launches_in_every_direction() {
        let spec = weapon_spec(PISTOL).expect("the pistol is in the official catalog");
        assert_eq!(spec.launch_type, 7);
    }

    /// Per-turn use limits are real in the upstream data (`max_uses`), so the
    /// pistol is capped at four shots a turn rather than the harness
    /// fixture's unlimited `-1`.
    #[test]
    fn the_official_pistol_is_capped_at_four_uses_a_turn() {
        let spec = weapon_spec(PISTOL).expect("the pistol is in the official catalog");
        assert_eq!(spec.max_uses, 4);
    }

    /// Every row converts — no unknown effect-type or area id anywhere in the
    /// three tables.
    #[test]
    fn every_official_row_converts() {
        assert!(!OFFICIAL_WEAPONS.is_empty());
        assert!(!OFFICIAL_CHIPS.is_empty());
        assert!(!OFFICIAL_BULBS.is_empty());
        for id in weapon_ids() {
            let spec = weapon_spec(id).expect("listed weapon id resolves");
            assert_eq!(spec.id, id);
        }
        for id in chip_ids() {
            let spec = chip_spec(id).expect("listed chip id resolves");
            assert_eq!(spec.id, id);
        }
        for id in bulb_ids() {
            let template = bulb_template(id).expect("listed bulb id resolves");
            assert_eq!(template.id, id);
        }
    }

    /// The tables' target/modifier masks all sit inside the known bits, so
    /// [`EffectTargets::from_bits_truncate`] drops nothing.
    #[test]
    fn official_masks_are_known_bits() {
        let rows = OFFICIAL_WEAPONS
            .iter()
            .map(|w| (w.item, w.effects))
            .chain(OFFICIAL_CHIPS.iter().map(|c| (c.item, c.effects)));
        for (item, effects) in rows {
            for e in effects {
                assert_eq!(
                    EffectTargets::from_bits_truncate(e.targets).bits(),
                    e.targets,
                    "item {item}: unknown target bits in {}",
                    e.targets
                );
                assert_eq!(
                    EffectModifiers::from_bits_truncate(e.modifiers).bits(),
                    e.modifiers,
                    "item {item}: unknown modifier bits in {}",
                    e.modifiers
                );
            }
        }
    }

    /// A summons template comes back with its stat ranges and granted chips.
    #[test]
    fn a_bulb_template_comes_back_for_a_summons_id() {
        let puny = bulb_template(1).expect("puny_bulb is template 1");
        assert_eq!(puny.name, "puny_bulb");
        assert_eq!(puny.life, (50, 300));
        assert_eq!(puny.tp, (4, 7));
        assert_eq!(puny.mp, (3, 5));
        assert_eq!(puny.chips, vec![21, 19, 3, 8]);
    }

    /// Unknown ids are `None`, not a panic.
    #[test]
    fn unknown_ids_are_absent() {
        assert!(weapon_spec(-1).is_none());
        assert!(chip_spec(-1).is_none());
        assert!(bulb_template(-1).is_none());
        assert!(weapon_unsupported_effect_types(-1).is_none());
        assert!(chip_unsupported_effect_types(-1).is_none());
    }

    /// No item in the shipped catalogs uses an effect type the official
    /// attack path would panic on — the whole catalog is castable today.
    #[test]
    fn no_official_item_uses_an_unported_effect_type() {
        for id in weapon_ids() {
            assert_eq!(
                weapon_unsupported_effect_types(id),
                Some(Vec::new()),
                "weapon {id}"
            );
        }
        for id in chip_ids() {
            assert_eq!(
                chip_unsupported_effect_types(id),
                Some(Vec::new()),
                "chip {id}"
            );
        }
    }
}
