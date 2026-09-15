//! Official item catalogs — the weapon, chip and bulb *templates* the
//! reference generator registers at startup (`Generator.loadWeapons` /
//! `loadChips` / `loadSummons`), read from the same
//! `data/{weapons,chips,summons}.json` it reads, vendored into this crate by
//! [`crate::catalog`].
//!
//! This is the official-model counterpart of [`crate::weapons`] /
//! [`crate::chips`]: those feed the legacy [`Fight`](crate::Fight) world,
//! while the official [`State`](crate::state::State) wants
//! [`WeaponSpec`](crate::state::WeaponSpec) /
//! [`ChipSpec`](crate::state::ChipSpec) /
//! [`BulbTemplate`](crate::state::BulbTemplate) values in its template maps.
//!
//! Each catalog is built once, on first lookup, and the accessors hand back a
//! clone — the spec types own their effect lines, and the fight registers
//! copies into its own maps anyway.

use std::sync::OnceLock;

use crate::attack::{Area, EffectModifiers, EffectParams, EffectTargets, EffectType};
use crate::catalog;
use crate::state::{BulbTemplate, ChipSpec, WeaponSpec};

// ─────────────────────────────────────────────────────────────────────────────
// JSON → spec
// ─────────────────────────────────────────────────────────────────────────────

/// One `effects` / `passive_effects` array as [`EffectParams`].
///
/// Panics on an effect-type id outside `Effect.TYPE_*`: the data is upstream's
/// own, so an unknown id means the snapshot grew a type [`EffectType`] doesn't
/// name yet, and silently dropping the line would make the item quietly weaker
/// than the real one.
fn effect_params(item: i32, entries: &[serde_json::Value]) -> Vec<EffectParams> {
    entries
        .iter()
        .map(|e| {
            let id = catalog::int32(e, "id", 0);
            EffectParams {
                effect: EffectType::from_id(id)
                    .unwrap_or_else(|| panic!("item {item}: unknown effect type id {id}")),
                value1: catalog::num(e, "value1"),
                value2: catalog::num(e, "value2"),
                turns: catalog::int32(e, "turns", 0),
                // Every mask in the upstream data is inside the known bits —
                // asserted by the `official_masks_are_known_bits` test, so the
                // truncation never actually drops one.
                targets: EffectTargets::from_bits_truncate(catalog::int32(e, "targets", 0)),
                modifiers: EffectModifiers::from_bits_truncate(catalog::int32(e, "modifiers", 0)),
            }
        })
        .collect()
}

/// The area id as an [`Area`], panicking on an id outside `Area.TYPE_*`.
fn area_of(item: i32, area: i32) -> Area {
    Area::from_id(area).unwrap_or_else(|| panic!("item {item}: unknown area id {area}"))
}

/// `Attack.getMaxUses()` when the entry omits the field.
///
/// `State.useWeapon`/`useChip` gate on `getMaxUses() != -1`, so a `0` default
/// would block every use of every item that does not declare a limit.
const UNLIMITED_USES: i64 = -1;

static WEAPONS: OnceLock<Vec<WeaponSpec>> = OnceLock::new();
static CHIPS: OnceLock<Vec<ChipSpec>> = OnceLock::new();
static BULBS: OnceLock<Vec<BulbTemplate>> = OnceLock::new();

fn weapons() -> &'static [WeaponSpec] {
    WEAPONS.get_or_init(|| {
        catalog::rows(catalog::WEAPONS_JSON, "weapons")
            .iter()
            .map(|w| {
                let item = catalog::int32(w, "item", 0);
                WeaponSpec {
                    id: item,
                    cost: catalog::int32(w, "cost", 0),
                    min_range: catalog::int32(w, "min_range", 0),
                    max_range: catalog::int32(w, "max_range", 0),
                    launch_type: catalog::int32(w, "launch_type", 0),
                    needs_los: catalog::flag(w, "los", true),
                    max_uses: catalog::int32(w, "max_uses", UNLIMITED_USES),
                    area: area_of(item, catalog::int32(w, "area", 1)),
                    effects: effect_params(item, catalog::entries(w, "effects")),
                    passive_effects: effect_params(item, catalog::entries(w, "passive_effects")),
                    forgotten: catalog::flag(w, "forgotten", false),
                }
            })
            .collect()
    })
}

fn chips() -> &'static [ChipSpec] {
    CHIPS.get_or_init(|| {
        catalog::rows(catalog::CHIPS_JSON, "chips")
            .iter()
            .map(|c| {
                let item = catalog::int32(c, "id", 0);
                ChipSpec {
                    id: item,
                    cost: catalog::int32(c, "cost", 0),
                    min_range: catalog::int32(c, "min_range", 0),
                    max_range: catalog::int32(c, "max_range", 0),
                    launch_type: catalog::int32(c, "launch_type", 0),
                    needs_los: catalog::flag(c, "los", true),
                    max_uses: catalog::int32(c, "max_uses", UNLIMITED_USES),
                    area: area_of(item, catalog::int32(c, "area", 1)),
                    effects: effect_params(item, catalog::entries(c, "effects")),
                    cooldown: catalog::int32(c, "cooldown", 0),
                    team_cooldown: catalog::flag(c, "team_cooldown", false),
                    initial_cooldown: catalog::int32(c, "initial_cooldown", 0),
                    level: catalog::int32(c, "level", 0),
                }
            })
            .collect()
    })
}

fn bulbs() -> &'static [BulbTemplate] {
    BULBS.get_or_init(|| {
        catalog::rows(catalog::SUMMONS_JSON, "summons")
            .iter()
            .map(|b| BulbTemplate {
                id: catalog::int32(b, "id", 0),
                name: catalog::text(b, "name"),
                life: catalog::range(b, "life"),
                strength: catalog::range(b, "strength"),
                wisdom: catalog::range(b, "wisdom"),
                agility: catalog::range(b, "agility"),
                resistance: catalog::range(b, "resistance"),
                science: catalog::range(b, "science"),
                magic: catalog::range(b, "magic"),
                tp: catalog::range(b, "tp"),
                mp: catalog::range(b, "mp"),
                chips: catalog::int_list(b, "chips"),
                states: catalog::int_list(b, "states"),
                zone: catalog::int32(b, "zone", 0),
            })
            .collect()
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// Lookups
// ─────────────────────────────────────────────────────────────────────────────

/// The official [`WeaponSpec`] for a public `WEAPON_*` item id, or `None`
/// when the catalog has no such weapon.
#[must_use]
pub fn weapon_spec(id: i32) -> Option<WeaponSpec> {
    weapons().iter().find(|w| w.id == id).cloned()
}

/// The official [`ChipSpec`] for a public `CHIP_*` item id, or `None` when
/// the catalog has no such chip.
#[must_use]
pub fn chip_spec(id: i32) -> Option<ChipSpec> {
    chips().iter().find(|c| c.id == id).cloned()
}

/// The official [`BulbTemplate`] for a summons template id, or `None` when
/// the catalog has no such bulb.
#[must_use]
pub fn bulb_template(id: i32) -> Option<BulbTemplate> {
    bulbs().iter().find(|b| b.id == id).cloned()
}

/// Every public weapon item id in the catalog, ascending.
pub fn weapon_ids() -> impl Iterator<Item = i32> {
    weapons().iter().map(|w| w.id)
}

/// Every public chip item id in the catalog, ascending.
pub fn chip_ids() -> impl Iterator<Item = i32> {
    chips().iter().map(|c| c.id)
}

/// Every bulb template id in the catalog, ascending.
pub fn bulb_ids() -> impl Iterator<Item = i32> {
    bulbs().iter().map(|b| b.id)
}

// ─────────────────────────────────────────────────────────────────────────────
// Effect coverage
// ─────────────────────────────────────────────────────────────────────────────

/// Whether the official attack path models this effect type.
///
/// Coverage grows corpus-first, so this is the inverse list — and it is empty:
/// every effect type any catalog item carries, as an active line or as a
/// passive one, is dispatched. Keep it in step with `State::create_effect`'s
/// `not ported yet` arm (plus the types intercepted before it:
/// `EffectType::Teleport` / `Propagation` in `State::apply_on_cell`, `Summon`
/// in `State::summon_entity` and `Resurrect` in `State::resurrect`) and with
/// the passive hooks in `State::activate_passive`.
fn is_ported(_effect: EffectType) -> bool {
    true
}

/// The effect types of this weapon the official attack path doesn't model
/// (empty = fully supported), or `None` when the catalog has no such weapon.
/// In item order, duplicates kept — the official-catalog counterpart of
/// [`crate::weapons::unsupported_effects`], for strict-mode validation.
///
/// Passive lines count: they are the weapon's too, they fire from the same
/// `State`, and leaving them out is how their whole family stayed invisible.
#[must_use]
pub fn weapon_unsupported_effect_types(id: i32) -> Option<Vec<EffectType>> {
    let w = weapons().iter().find(|w| w.id == id)?;
    Some(unsupported_of(w.effects.iter().chain(&w.passive_effects)))
}

/// The effect types of this chip the official attack path doesn't model
/// (empty = fully supported), or `None` when the catalog has no such chip.
#[must_use]
pub fn chip_unsupported_effect_types(id: i32) -> Option<Vec<EffectType>> {
    let c = chips().iter().find(|c| c.id == id)?;
    Some(unsupported_of(c.effects.iter()))
}

fn unsupported_of<'a>(effects: impl Iterator<Item = &'a EffectParams>) -> Vec<EffectType> {
    effects
        .map(|e| e.effect)
        .filter(|&t| !is_ported(t))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `WEAPON_PISTOL`.
    const PISTOL: i32 = 37;

    /// The catalog pistol still carries the hand-written harness pistol's
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
    /// three catalogs.
    #[test]
    fn every_official_row_converts() {
        assert!(!weapons().is_empty());
        assert!(!chips().is_empty());
        assert!(!bulbs().is_empty());
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

    /// The masks all sit inside the known bits, so
    /// [`EffectTargets::from_bits_truncate`] drops nothing.
    ///
    /// Reading them back off the parsed specs would prove nothing — the
    /// truncation has already happened by then — so this goes to the JSON.
    #[test]
    fn official_masks_are_known_bits() {
        let files = [
            (catalog::WEAPONS_JSON, "weapons"),
            (catalog::CHIPS_JSON, "chips"),
        ];
        for (json, what) in files {
            for row in catalog::rows(json, what) {
                let item = catalog::int(&row, "item", catalog::int(&row, "id", 0));
                let lines = catalog::entries(&row, "effects")
                    .iter()
                    .chain(catalog::entries(&row, "passive_effects"));
                for e in lines {
                    let targets = catalog::int32(e, "targets", 0);
                    let modifiers = catalog::int32(e, "modifiers", 0);
                    assert_eq!(
                        EffectTargets::from_bits_truncate(targets).bits(),
                        targets,
                        "item {item}: unknown target bits in {targets}",
                    );
                    assert_eq!(
                        EffectModifiers::from_bits_truncate(modifiers).bits(),
                        modifiers,
                        "item {item}: unknown modifier bits in {modifiers}",
                    );
                }
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

    /// The 2.50 plants are summons templates like the bulbs, and carry two
    /// fields no bulb does: the states they start rooted with, and the radius
    /// of their awakening zone. A transcriber taught only about bulbs dropped
    /// both, which is the argument for reading upstream's own file.
    #[test]
    fn a_plant_template_carries_its_states_and_awakening_zone() {
        let corn = bulb_template(9).expect("corn is template 9");
        assert_eq!(corn.name, "corn");
        assert_eq!(corn.states, vec![9], "ROOTED");
        assert_eq!(corn.zone, 3);

        let prototaxite = bulb_template(13).expect("prototaxite is template 13");
        assert_eq!(prototaxite.states, vec![9]);
        assert_eq!(
            prototaxite.zone, 0,
            "the prototaxite is rooted but wakes nobody"
        );

        let puny = bulb_template(1).expect("puny_bulb is template 1");
        assert!(puny.states.is_empty(), "a bulb is not rooted");
        assert_eq!(puny.zone, 0);
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
