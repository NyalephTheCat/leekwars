//! Attack application — ports of `attack/Attack.java` (`applyOnCell`, target
//! filtering, area attenuation) and `effect/Effect.java`'s `createEffect`
//! factory with the effect types the corpus exercises (currently
//! `EffectDamage`, `EffectPoison`, `EffectHeal`, `EffectBuffStrength`, the
//! shields, the TP/MP shackles and `EffectDamageReturn`), including the
//! timed-effect machinery (remove-previous, stack-merge, store + expiry).
//!
//! Like [`crate::official_builtins`], coverage grows corpus-first: area and
//! effect types that no golden exercises yet **panic** instead of silently
//! diverging, so a new scenario immediately points at the missing port.

use bitflags::bitflags;

use crate::actions::{ATTACK_TYPE_CHIP, ATTACK_TYPE_WEAPON, Action, DamageType};
use crate::state::{
    Fighter, STAT_ABSOLUTE_SHIELD, STAT_AGILITY, STAT_COUNT, STAT_DAMAGE_RETURN, STAT_FREQUENCY,
    STAT_LIFE, STAT_MAGIC, STAT_MP, STAT_POWER, STAT_RELATIVE_SHIELD, STAT_RESISTANCE,
    STAT_SCIENCE, STAT_STRENGTH, STAT_TP, STAT_WISDOM, State, Stats,
};

// ─────────────────────────────────────────────────────────────────────────────
// Effect model  (effect/Effect.java constants, effect/EffectParameters.java)
// ─────────────────────────────────────────────────────────────────────────────

/// `Effect.TYPE_*` — every effect type of the reference generator, with the
/// official 1-based ids as discriminants. Which ones are *applied* is
/// corpus-driven (see [`State::apply_on_cell`]); the full list exists so
/// scenario data can always be parsed and named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum EffectType {
    Damage = 1,
    Heal = 2,
    BuffStrength = 3,
    BuffAgility = 4,
    RelativeShield = 5,
    AbsoluteShield = 6,
    BuffMp = 7,
    BuffTp = 8,
    Debuff = 9,
    Teleport = 10,
    Permutation = 11,
    Vitality = 12,
    Poison = 13,
    Summon = 14,
    Resurrect = 15,
    Kill = 16,
    ShackleMp = 17,
    ShackleTp = 18,
    ShackleStrength = 19,
    DamageReturn = 20,
    BuffResistance = 21,
    BuffWisdom = 22,
    Antidote = 23,
    ShackleMagic = 24,
    Aftereffect = 25,
    Vulnerability = 26,
    AbsoluteVulnerability = 27,
    LifeDamage = 28,
    StealAbsoluteShield = 29,
    NovaDamage = 30,
    RawBuffMp = 31,
    RawBuffTp = 32,
    PoisonToScience = 33,
    DamageToAbsoluteShield = 34,
    DamageToStrength = 35,
    NovaDamageToMagic = 36,
    RawAbsoluteShield = 37,
    RawBuffStrength = 38,
    RawBuffMagic = 39,
    RawBuffScience = 40,
    RawBuffAgility = 41,
    RawBuffResistance = 42,
    Propagation = 43,
    RawBuffWisdom = 44,
    NovaVitality = 45,
    Attract = 46,
    ShackleAgility = 47,
    ShackleWisdom = 48,
    RemoveShackles = 49,
    MovedToMp = 50,
    Push = 51,
    RawBuffPower = 52,
    Repel = 53,
    RawRelativeShield = 54,
    AllyKilledToAgility = 55,
    KillToTp = 56,
    RawHeal = 57,
    CriticalToHeal = 58,
    AddState = 59,
    TotalDebuff = 60,
    StealLife = 61,
    MultiplyStats = 62,
}

impl EffectType {
    /// The official wire id (`Effect.getId()`).
    #[must_use]
    pub fn id(self) -> i32 {
        self as i32
    }

    /// Parse an official effect id (scenario / item data).
    #[must_use]
    pub fn from_id(id: i32) -> Option<Self> {
        // The discriminants are exactly 1..=62 with no gaps — round-tripped
        // by the `effect_type_ids` test.
        Some(match id {
            1 => Self::Damage,
            2 => Self::Heal,
            3 => Self::BuffStrength,
            4 => Self::BuffAgility,
            5 => Self::RelativeShield,
            6 => Self::AbsoluteShield,
            7 => Self::BuffMp,
            8 => Self::BuffTp,
            9 => Self::Debuff,
            10 => Self::Teleport,
            11 => Self::Permutation,
            12 => Self::Vitality,
            13 => Self::Poison,
            14 => Self::Summon,
            15 => Self::Resurrect,
            16 => Self::Kill,
            17 => Self::ShackleMp,
            18 => Self::ShackleTp,
            19 => Self::ShackleStrength,
            20 => Self::DamageReturn,
            21 => Self::BuffResistance,
            22 => Self::BuffWisdom,
            23 => Self::Antidote,
            24 => Self::ShackleMagic,
            25 => Self::Aftereffect,
            26 => Self::Vulnerability,
            27 => Self::AbsoluteVulnerability,
            28 => Self::LifeDamage,
            29 => Self::StealAbsoluteShield,
            30 => Self::NovaDamage,
            31 => Self::RawBuffMp,
            32 => Self::RawBuffTp,
            33 => Self::PoisonToScience,
            34 => Self::DamageToAbsoluteShield,
            35 => Self::DamageToStrength,
            36 => Self::NovaDamageToMagic,
            37 => Self::RawAbsoluteShield,
            38 => Self::RawBuffStrength,
            39 => Self::RawBuffMagic,
            40 => Self::RawBuffScience,
            41 => Self::RawBuffAgility,
            42 => Self::RawBuffResistance,
            43 => Self::Propagation,
            44 => Self::RawBuffWisdom,
            45 => Self::NovaVitality,
            46 => Self::Attract,
            47 => Self::ShackleAgility,
            48 => Self::ShackleWisdom,
            49 => Self::RemoveShackles,
            50 => Self::MovedToMp,
            51 => Self::Push,
            52 => Self::RawBuffPower,
            53 => Self::Repel,
            54 => Self::RawRelativeShield,
            55 => Self::AllyKilledToAgility,
            56 => Self::KillToTp,
            57 => Self::RawHeal,
            58 => Self::CriticalToHeal,
            59 => Self::AddState,
            60 => Self::TotalDebuff,
            61 => Self::StealLife,
            62 => Self::MultiplyStats,
            _ => return None,
        })
    }
}

/// `EntityState` — the states an `EffectAddState` effect can pin on an
/// entity. Discriminants are the Java enum ordinals (`EntityState.values()`
/// is indexed by `value1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityState {
    None = 0,
    Resurrected = 1,
    Unhealable = 2,
    Invincible = 3,
    Pacifist = 4,
    Heavy = 5,
    Dense = 6,
    Magnetized = 7,
    Chained = 8,
    Rooted = 9,
    Petrified = 10,
    Static = 11,
}

impl EntityState {
    /// `EntityState.values()[ordinal]` — panics on out-of-range like Java.
    #[must_use]
    pub fn from_ordinal(ordinal: i32) -> Self {
        match ordinal {
            0 => Self::None,
            1 => Self::Resurrected,
            2 => Self::Unhealable,
            3 => Self::Invincible,
            4 => Self::Pacifist,
            5 => Self::Heavy,
            6 => Self::Dense,
            7 => Self::Magnetized,
            8 => Self::Chained,
            9 => Self::Rooted,
            10 => Self::Petrified,
            11 => Self::Static,
            other => panic!("EntityState ordinal {other} out of range"),
        }
    }
}

bitflags! {
    /// `Effect.TARGET_*` — who an effect line may hit.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EffectTargets: i32 {
        const ENEMIES = 1;
        const ALLIES = 2;
        const CASTER = 4;
        const NON_SUMMONS = 8;
        const SUMMONS = 16;
    }
}

bitflags! {
    /// `Effect.MODIFIER_*` — how an effect line applies.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct EffectModifiers: i32 {
        const STACKABLE = 1;
        const MULTIPLIED_BY_TARGETS = 2;
        const ON_CASTER = 4;
        const NOT_REPLACEABLE = 8;
        const IRREDUCTIBLE = 16;
    }
}

/// One effect line of an attack (`EffectParameters`) — the per-item template
/// data, not a live effect.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectParams {
    pub effect: EffectType,
    pub value1: f64,
    pub value2: f64,
    pub turns: i32,
    pub targets: EffectTargets,
    pub modifiers: EffectModifiers,
}

/// `Attack.TYPE_WEAPON` / `Attack.TYPE_CHIP` — what kind of item carries an
/// attack. Drives the `ActionAddEffect` action-type remap (301 / 302).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttackType {
    Weapon,
    Chip,
}

impl AttackType {
    /// The raw `Attack.getType()` value, as `ActionAddEffect.createEffect`
    /// receives it.
    #[must_use]
    fn action_type(self) -> i64 {
        match self {
            Self::Weapon => ATTACK_TYPE_WEAPON,
            Self::Chip => ATTACK_TYPE_CHIP,
        }
    }
}

/// One live effect (`Effect.java`'s instance state) — an entry in the
/// [`State::effects`] arena. Entries are never compacted: membership truth is
/// the fighters' `effects` / `launched_effects` index lists, so a removed
/// effect simply goes stale in the arena.
#[derive(Debug, Clone)]
pub struct EffectInstance {
    pub effect: EffectType,
    /// `attack.getItemId()` — the weapon/chip template that cast it.
    pub item_id: i32,
    pub attack_type: AttackType,
    /// Remaining turns (`-1` = whole fight), decremented at the *caster's*
    /// turn start.
    pub turns: i32,
    /// Computed effect value (`Effect.value`) — grows on merge.
    pub value: i32,
    pub erosion_rate: f64,
    pub critical: bool,
    pub caster: usize,
    pub target: usize,
    /// Stat deltas this effect contributes (`Effect.stats`) — summed into the
    /// target's buff stats by [`State::update_buff_stats`].
    pub stats: Stats,
    /// Client-facing effect id assigned by `ActionAddEffect` (`Effect.logID`).
    pub log_id: u32,
    pub modifiers: EffectModifiers,
    /// `Effect.propagate` — propagation distance; effects with a nonzero
    /// distance re-spread from their target at that entity's turn end.
    pub propagate: i32,
    /// `Effect.state` — the entity state an `EffectAddState` effect carries;
    /// rebuilt into the target's state set by [`State::update_buff_stats`].
    pub state: Option<EntityState>,
}

/// `Effect.CRITICAL_FACTOR`.
pub const CRITICAL_FACTOR: f64 = 1.3;
/// `Effect.EROSION_DAMAGE`.
pub const EROSION_DAMAGE: f64 = 0.05;
/// `Effect.EROSION_POISON`.
pub const EROSION_POISON: f64 = 0.10;
/// `Effect.EROSION_CRITICAL_BONUS`.
pub const EROSION_CRITICAL_BONUS: f64 = 0.10;

// ─────────────────────────────────────────────────────────────────────────────
// Areas  (area/Area.java)
// ─────────────────────────────────────────────────────────────────────────────

/// `Area.TYPE_*` — the shape an attack covers around its target cell, with
/// the official ids as discriminants. (`TYPE_AREA_PLUS_1` is an alias of
/// `TYPE_CIRCLE1` upstream and maps to [`Area::Circle1`].)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum Area {
    SingleCell = 1,
    LaserLine = 2,
    Circle1 = 3,
    Circle2 = 4,
    Circle3 = 5,
    Plus2 = 6,
    Plus3 = 7,
    X1 = 8,
    X2 = 9,
    X3 = 10,
    Square1 = 11,
    Square2 = 12,
    FirstInLine = 13,
    Enemies = 14,
    Allies = 15,
}

impl Area {
    /// The official wire id.
    #[must_use]
    pub fn id(self) -> i32 {
        self as i32
    }

    /// Parse an official area id (scenario / item data).
    #[must_use]
    pub fn from_id(id: i32) -> Option<Self> {
        Some(match id {
            1 => Self::SingleCell,
            2 => Self::LaserLine,
            3 => Self::Circle1,
            4 => Self::Circle2,
            5 => Self::Circle3,
            6 => Self::Plus2,
            7 => Self::Plus3,
            8 => Self::X1,
            9 => Self::X2,
            10 => Self::X3,
            11 => Self::Square1,
            12 => Self::Square2,
            13 => Self::FirstInLine,
            14 => Self::Enemies,
            15 => Self::Allies,
            _ => return None,
        })
    }

    /// Whether `Attack.getPowerForCell` attenuates with distance: the
    /// line/entity areas always hit at full power.
    #[must_use]
    fn attenuates(self) -> bool {
        !matches!(
            self,
            Self::LaserLine | Self::FirstInLine | Self::Allies | Self::Enemies
        )
    }

    /// The `MaskAreaCell` offset list backing this area's `MaskArea`, in the
    /// exact generation order (it drives the target-entity order, which is
    /// observable through the action log). `None` for the non-mask areas.
    #[must_use]
    fn mask(self) -> Option<Vec<(i32, i32)>> {
        Some(match self {
            Self::Circle1 => circle_mask(0, 1),
            Self::Circle2 => circle_mask(0, 2),
            Self::Circle3 => circle_mask(0, 3),
            Self::Plus2 => plus_mask(2),
            Self::Plus3 => plus_mask(3),
            Self::X1 => x_mask(1),
            Self::X2 => x_mask(2),
            Self::X3 => x_mask(3),
            Self::Square1 => square_mask(1),
            Self::Square2 => square_mask(2),
            Self::SingleCell
            | Self::LaserLine
            | Self::FirstInLine
            | Self::Enemies
            | Self::Allies => return None,
        })
    }
}

/// `MaskAreaCell.generateCircleMask(min, max)` — center (when `min == 0`),
/// then each ring counter-clockwise from the east corner.
fn circle_mask(min: i32, max: i32) -> Vec<(i32, i32)> {
    let mut cells = Vec::new();
    if min == 0 {
        cells.push((0, 0));
    }
    for size in min.max(1)..=max {
        for i in 0..size {
            cells.push((size - i, -i));
        }
        for i in 0..size {
            cells.push((-i, -(size - i)));
        }
        for i in 0..size {
            cells.push((-(size - i), i));
        }
        for i in 0..size {
            cells.push((i, size - i));
        }
    }
    cells
}

/// `MaskAreaCell.generatePlusMask(radius)`.
fn plus_mask(radius: i32) -> Vec<(i32, i32)> {
    let mut cells = vec![(0, 0)];
    for size in 1..=radius {
        cells.push((size, 0));
        cells.push((0, -size));
        cells.push((-size, 0));
        cells.push((0, size));
    }
    cells
}

/// `MaskAreaCell.generateXMask(radius)`.
fn x_mask(radius: i32) -> Vec<(i32, i32)> {
    let mut cells = vec![(0, 0)];
    for size in 1..=radius {
        cells.push((size, -size));
        cells.push((-size, -size));
        cells.push((-size, size));
        cells.push((size, size));
    }
    cells
}

/// `MaskAreaCell.generateSquareMask(radius)` — the inscribed circle, then
/// the corners counter-clockwise.
fn square_mask(radius: i32) -> Vec<(i32, i32)> {
    let mut cells = circle_mask(0, radius);
    for d in 0..radius {
        for i in 1..=(radius - d) {
            cells.push((radius + 1 - i, -(d + i)));
        }
        for i in 1..=(radius - d) {
            cells.push((-(d + i), -(radius + 1 - i)));
        }
        for i in 1..=(radius - d) {
            cells.push((-(radius + 1 - i), d + i));
        }
        for i in 1..=(radius - d) {
            cells.push((d + i, radius + 1 - i));
        }
    }
    cells
}

/// `(int) Math.round(double)` — Java rounds ties toward +∞ and, unlike the
/// naive `floor(x + 0.5)`, is immune to the FP-addition error (JDK-8010430:
/// `0.49999999999999994 + 0.5 == 1.0` but `Math.round` returns 0). Rust's
/// `f64::round` is half-away-from-zero, which disagrees on negative ties.
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn java_round(a: f64) -> i32 {
    let floor = a.floor();
    if a - floor >= 0.5 {
        floor as i32 + 1
    } else {
        floor as i32
    }
}

/// `Attack.filterTarget(targets, caster, target)` — leek scope: no summons,
/// so `target.isSummon()` is always false.
#[must_use]
fn filter_target(targets: EffectTargets, caster: &Fighter, target: &Fighter) -> bool {
    if !targets.contains(EffectTargets::ENEMIES) && caster.team != target.team {
        return false;
    }
    if !targets.contains(EffectTargets::ALLIES) && caster.team == target.team {
        return false;
    }
    if !targets.contains(EffectTargets::CASTER) && caster.fid == target.fid {
        return false;
    }
    // Non-summons: every leek is a non-summon, so an attack that can't hit
    // non-summons hits nothing here. (Summons bit never filters: no summons.)
    if !targets.contains(EffectTargets::NON_SUMMONS) {
        return false;
    }
    true
}

impl State {
    /// `Attack.getPowerForCell(target_cell, current_cell)` — the area
    /// attenuation: 100% at the center, −20% per case of distance.
    #[must_use]
    fn power_for_cell(&self, area: Area, target_cell: usize, current_cell: usize) -> f64 {
        if !area.attenuates() {
            return 1.0;
        }
        let t = &self.map.cells[target_cell];
        let c = &self.map.cells[current_cell];
        let dist = f64::from((t.x - c.x).abs() + (t.y - c.y).abs());
        1.0 - dist * 0.2
    }

    /// `Area.getArea(map, launch_cell, target_cell, caster)` for the attack's
    /// area type.
    #[allow(clippy::too_many_arguments)]
    fn area_cells(
        &self,
        area: Area,
        caster: usize,
        launch_cell: usize,
        target_cell: usize,
        min_range: i32,
        max_range: i32,
        needs_los: bool,
    ) -> Vec<usize> {
        if let Some(mask) = area.mask() {
            // `MaskArea.getArea` — offsets around the target, skipping
            // missing and obstacle cells.
            let t = &self.map.cells[target_cell];
            return mask
                .into_iter()
                .filter_map(|(ox, oy)| self.map.get_cell_xy(t.x + ox, t.y + oy))
                .filter(|&c| self.map.cells[c].walkable)
                .collect();
        }
        match area {
            Area::SingleCell => vec![target_cell],
            Area::LaserLine => {
                // `AreaLaserLine.getArea` — the launch ray, cut at the map
                // edge and (for LoS attacks) at the first obstacle.
                let l = &self.map.cells[launch_cell];
                let t = &self.map.cells[target_cell];
                let (dx, dy) = if l.x == t.x {
                    (0, if l.y > t.y { -1 } else { 1 })
                } else if l.y == t.y {
                    (if l.x > t.x { -1 } else { 1 }, 0)
                } else {
                    return Vec::new();
                };
                let mut cells = Vec::new();
                for i in min_range..=max_range {
                    let Some(c) = self.map.get_cell_xy(l.x + dx * i, l.y + dy * i) else {
                        break;
                    };
                    if needs_los && !self.map.cells[c].walkable {
                        break;
                    }
                    cells.push(c);
                }
                cells
            }
            Area::FirstInLine => {
                // `AreaFirstInLine.getArea` — the first occupied cell on the
                // signum ray, if any.
                self.map
                    .get_first_entity(launch_cell, target_cell, min_range, max_range)
                    .into_iter()
                    .collect()
            }
            Area::Enemies | Area::Allies => {
                // `AreaEnemies`/`AreaAllies.getArea` — every cell holding an
                // entity of the other/own team, wherever it stands (the cast
                // cell is ignored). Java iterates `state.getEntities()`, a
                // HashMap keyed by fid — ascending for small int keys.
                // (`AreaAllies` skips own-team "crystal" entities — none in
                // leek scope.)
                let team = self.fighters[caster].team;
                self.fighters
                    .iter()
                    .filter(|f| {
                        if area == Area::Enemies {
                            f.team != team
                        } else {
                            f.team == team
                        }
                    })
                    .filter_map(|f| f.cell)
                    .collect()
            }
            // The mask areas (circles/plus/X/squares) returned above.
            other => unreachable!("mask area {other:?} is handled by the mask path"),
        }
    }

    /// `Attack.applyOnCell(state, caster, target, critical)` — resolve the
    /// area, draw the jet, and run every effect line of the attack. Returns
    /// the affected fids in Java's `returnEntities` order (duplicates and
    /// all — `useWeapon` only uses it for statistics, but chips will need
    /// the exact list).
    #[allow(clippy::too_many_arguments)]
    pub fn apply_on_cell(
        &mut self,
        caster: usize,
        target_cell: usize,
        critical: bool,
        area: Area,
        effects: &[EffectParams],
        item_id: i32,
        attack_type: AttackType,
        min_range: i32,
        max_range: i32,
        needs_los: bool,
    ) -> Vec<usize> {
        let Some(launch_cell) = self.fighters[caster].cell else {
            return Vec::new();
        };
        let mut return_entities: Vec<usize> = Vec::new();

        // Target cells → living entities on them, with their area factors
        // (frozen at the pre-slide positions, like Java's `areaFactors`).
        let target_cells = self.area_cells(
            area,
            caster,
            launch_cell,
            target_cell,
            min_range,
            max_range,
            needs_los,
        );
        let mut target_entities: Vec<usize> = Vec::new();
        let mut area_factors: Vec<f64> = Vec::new();
        for cell in target_cells {
            if let Some(fid) = self.entity_on(cell)
                && !self.fighters[fid].is_dead()
            {
                target_entities.push(fid);
                area_factors.push(self.power_for_cell(area, target_cell, cell));
            }
        }

        // One jet per applyOnCell call, drawn before any effect runs.
        let jet = self.rng.get_double();

        let mut previous_effect_total_value = 0;
        let mut propagate = 0;

        for params in effects {
            if self.fighters[caster].is_dead() {
                continue;
            }

            // Slides run on ALL collected target entities (no target filter),
            // from their LIVE cells; the matching Attract/Push effect line
            // still falls through to the generic branch below (where the base
            // `Effect.apply` keeps its value at 0, so nothing is stored).
            if params.effect == EffectType::Attract {
                for &fid in &target_entities {
                    if let Some(cell) = self.fighters[fid].cell {
                        let dest =
                            self.map
                                .attract_last_available_cell(cell, target_cell, launch_cell);
                        self.slide_entity(fid, dest);
                    }
                }
            } else if params.effect == EffectType::Push {
                for &fid in &target_entities {
                    if let Some(cell) = self.fighters[fid].cell {
                        let dest =
                            self.map
                                .push_last_available_cell(cell, target_cell, launch_cell);
                        self.slide_entity(fid, dest);
                    }
                }
            }

            match params.effect {
                EffectType::Teleport => {
                    self.teleport_entity(caster, target_cell);
                    // Java adds the caster unconditionally (no dedup).
                    return_entities.push(caster);
                }
                EffectType::Propagation => {
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        propagate = params.value1 as i32;
                    }
                }
                _ => {
                    let on_caster = params.modifiers.contains(EffectModifiers::ON_CASTER);
                    let stackable = params.modifiers.contains(EffectModifiers::STACKABLE);
                    let multiplied = params
                        .modifiers
                        .contains(EffectModifiers::MULTIPLIED_BY_TARGETS);
                    let not_replaceable =
                        params.modifiers.contains(EffectModifiers::NOT_REPLACEABLE);

                    let mut effect_targets: Vec<(usize, f64)> = Vec::new();
                    for (&fid, &aoe) in target_entities.iter().zip(&area_factors) {
                        if self.fighters[fid].is_dead() {
                            continue;
                        }
                        if !filter_target(
                            params.targets,
                            &self.fighters[caster],
                            &self.fighters[fid],
                        ) {
                            continue;
                        }
                        if on_caster && fid == caster {
                            continue;
                        }
                        // The effect is already on the target and can't be
                        // replaced.
                        if not_replaceable && self.has_effect(fid, item_id) {
                            continue;
                        }
                        if !return_entities.contains(&fid) {
                            return_entities.push(fid);
                        }
                        effect_targets.push((fid, aoe));
                    }
                    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                    let target_count = if multiplied {
                        effect_targets.len() as i32
                    } else {
                        1
                    };

                    let mut effect_total_value = 0;
                    if on_caster {
                        // Java adds the caster unconditionally here (possible
                        // duplicate — verbatim).
                        return_entities.push(caster);
                        self.create_effect(
                            params,
                            1.0,
                            critical,
                            caster,
                            caster,
                            item_id,
                            attack_type,
                            jet,
                            stackable,
                            previous_effect_total_value,
                            target_count,
                            propagate,
                        );
                    } else {
                        for (fid, aoe) in effect_targets {
                            effect_total_value += self.create_effect(
                                params,
                                aoe,
                                critical,
                                fid,
                                caster,
                                item_id,
                                attack_type,
                                jet,
                                stackable,
                                previous_effect_total_value,
                                target_count,
                                propagate,
                            );
                        }
                    }
                    previous_effect_total_value = effect_total_value;
                }
            }
        }
        return_entities
    }

    /// `Entity.hasEffect(attackID)` — any active effect cast by this item.
    #[must_use]
    fn has_effect(&self, fid: usize, item_id: i32) -> bool {
        self.fighters[fid]
            .effects
            .iter()
            .any(|&ei| self.effects[ei].item_id == item_id)
    }

    /// `Entity.updateBuffStats()` — rebuild the buff stats AND the entity
    /// state set from the active effects (so an expired/removed
    /// `EffectAddState` drops its state here).
    pub fn update_buff_stats(&mut self, fid: usize) {
        let mut stats = Stats::default();
        let mut states = Vec::new();
        for &ei in &self.fighters[fid].effects {
            stats.add(&self.effects[ei].stats);
            if let Some(state) = self.effects[ei].state {
                states.push(state);
            }
        }
        self.fighters[fid].buff_stats = stats;
        self.fighters[fid].states = states;
    }

    /// `Entity.removeEffect(effect)` — log `ActionRemoveEffect`, drop the
    /// effect from the target's list and rebuild its buff stats. (Does *not*
    /// touch the caster's `launched_effects` — callers do, like Java.)
    pub fn remove_effect(&mut self, target: usize, ei: usize) {
        self.actions.log(Action::RemoveEffect {
            log_id: self.effects[ei].log_id,
        });
        self.fighters[target].effects.retain(|&i| i != ei);
        self.update_buff_stats(target);
    }

    /// `Effect.reduce(percent, caster)` — scale the effect's value and every
    /// carried stat by `1 - percent` (each with its own rounding: the value
    /// uses Java `Math.round`, the stats round on the absolute value and
    /// re-apply the sign).
    fn reduce_effect(&mut self, ei: usize, percent: f64) {
        let reduction = (1.0 - percent).max(0.0);
        let target = self.effects[ei].target;
        self.effects[ei].value = java_round(f64::from(self.effects[ei].value) * reduction);
        for stat in 0..STAT_COUNT {
            let v = self.effects[ei].stats.get(stat);
            if v == 0 {
                continue; // signum(0) zeroes the term in Java anyway
            }
            let new_value = java_round(f64::from(v.abs()) * reduction) * v.signum();
            let delta = new_value - v;
            self.effects[ei].stats.update(stat, delta);
            self.fighters[target].buff_stats.update(stat, delta);
        }
    }

    /// `Entity.reduceEffects(percent, caster)` — reduce every non-IRREDUCTIBLE
    /// effect; drop the ones whose value reaches 0 (logs `[303]`), log
    /// `[304]` updates for the survivors, then rebuild the buff stats.
    pub fn reduce_effects(&mut self, target: usize, percent: f64) {
        let mut i = 0;
        while i < self.fighters[target].effects.len() {
            let ei = self.fighters[target].effects[i];
            if self.effects[ei]
                .modifiers
                .contains(EffectModifiers::IRREDUCTIBLE)
            {
                i += 1;
                continue;
            }
            self.reduce_effect(ei, percent);
            if self.effects[ei].value <= 0 {
                let caster = self.effects[ei].caster;
                self.fighters[caster].launched_effects.retain(|&x| x != ei);
                self.remove_effect(target, ei); // i stays — the list shrank
            } else {
                let (log_id, value) = (self.effects[ei].log_id, self.effects[ei].value);
                self.actions.log(Action::UpdateEffect { log_id, value });
                i += 1;
            }
        }
        self.update_buff_stats(target);
    }

    /// `Entity.reduceEffectsTotal(percent, caster)` — like
    /// [`Self::reduce_effects`] but WITHOUT the IRREDUCTIBLE skip: every
    /// effect is reduced.
    pub fn reduce_effects_total(&mut self, target: usize, percent: f64) {
        let mut i = 0;
        while i < self.fighters[target].effects.len() {
            let ei = self.fighters[target].effects[i];
            self.reduce_effect(ei, percent);
            if self.effects[ei].value <= 0 {
                let caster = self.effects[ei].caster;
                self.fighters[caster].launched_effects.retain(|&x| x != ei);
                self.remove_effect(target, ei); // i stays — the list shrank
            } else {
                let (log_id, value) = (self.effects[ei].log_id, self.effects[ei].value);
                self.actions.log(Action::UpdateEffect { log_id, value });
                i += 1;
            }
        }
        self.update_buff_stats(target);
    }

    /// `Entity.clearPoisons(caster)` — remove every poison effect (each via
    /// `removeEffect`, which logs `[303]`); statistics-only beyond that.
    pub fn clear_poisons(&mut self, target: usize) {
        let mut i = 0;
        while i < self.fighters[target].effects.len() {
            let ei = self.fighters[target].effects[i];
            if self.effects[ei].effect == EffectType::Poison {
                let caster = self.effects[ei].caster;
                self.fighters[caster].launched_effects.retain(|&x| x != ei);
                self.remove_effect(target, ei);
            } else {
                i += 1;
            }
        }
    }

    /// `Entity.removeShackles()` — remove every shackle-family effect (each
    /// via `removeEffect`, which logs `[303]`).
    pub fn remove_shackles(&mut self, target: usize) {
        let mut i = 0;
        while i < self.fighters[target].effects.len() {
            let ei = self.fighters[target].effects[i];
            if matches!(
                self.effects[ei].effect,
                EffectType::ShackleTp
                    | EffectType::ShackleMp
                    | EffectType::ShackleAgility
                    | EffectType::ShackleMagic
                    | EffectType::ShackleStrength
                    | EffectType::ShackleWisdom
            ) {
                let caster = self.effects[ei].caster;
                self.fighters[caster].launched_effects.retain(|&x| x != ei);
                self.remove_effect(target, ei);
            } else {
                i += 1;
            }
        }
    }

    /// `Effect.applyStartTurn(state)` — the per-turn tick of an active
    /// effect, run at its *target's* turn start (poison damages, heal-over-
    /// time heals; the default is a no-op).
    #[allow(clippy::cast_possible_wrap)]
    pub fn apply_start_turn_effect(&mut self, ei: usize) {
        match self.effects[ei].effect {
            EffectType::Poison => {
                let (target, caster, value, erosion_rate) = {
                    let e = &self.effects[ei];
                    (e.target, e.caster, e.value, e.erosion_rate)
                };
                let mut damages = value.min(self.fighters[target].life);
                // An INVINCIBLE target zeroes the tick — and unlike direct
                // damage, a 0 poison tick logs NOTHING.
                if self.fighters[target].has_state(EntityState::Invincible) {
                    damages = 0;
                }
                if damages > 0 {
                    let target_id = target as i64;
                    let erosion = java_round(f64::from(damages) * erosion_rate);
                    self.actions.log(Action::Damage {
                        damage_type: DamageType::Poison,
                        target_id,
                        pv: damages,
                        erosion,
                    });
                    self.remove_life(target, damages, erosion, Some(caster));
                    // onPoisonDamage / onNovaDamage: passive effects — none
                    // in the leek scope.
                }
            }
            EffectType::Heal => {
                // `EffectHeal.applyStartTurn` — same cap + log + addLife as
                // the instant heal (log even at 0, like Java), but an
                // UNHEALABLE target skips the tick entirely (no log).
                if self.fighters[self.effects[ei].target].has_state(EntityState::Unhealable) {
                    return;
                }
                let (target, value) = {
                    let e = &self.effects[ei];
                    (e.target, e.value)
                };
                let t = &self.fighters[target];
                let life = value.min(t.total_life - t.life);
                self.actions.log(Action::Heal {
                    target_id: target as i64,
                    life,
                });
                self.add_life(target, life);
            }
            EffectType::Aftereffect => {
                // `EffectAftereffect.applyStartTurn` — re-clamp to the
                // target's life (the clamp PERSISTS on the stored effect),
                // then log + removeLife UNCONDITIONALLY: unlike poison there
                // is no INVINCIBLE check and a 0 tick still logs.
                let (target, caster, erosion_rate) = {
                    let e = &self.effects[ei];
                    (e.target, e.caster, e.erosion_rate)
                };
                let value = self.effects[ei].value.min(self.fighters[target].life);
                self.effects[ei].value = value;
                let erosion = java_round(f64::from(value) * erosion_rate);
                self.actions.log(Action::Damage {
                    damage_type: DamageType::Poison,
                    target_id: target as i64,
                    pv: value,
                    erosion,
                });
                self.remove_life(target, value, erosion, Some(caster));
            }
            // Every other ported effect keeps the default no-op
            // `applyStartTurn`.
            _ => {}
        }
    }

    /// `Entity.endTurn()`'s propagation block — every effect on `fid` with a
    /// propagation distance re-spreads to the entities around it (one jet per
    /// propagating effect, original effect line, full power, the stored
    /// critical flag, and the original caster).
    pub(crate) fn propagate_effects(&mut self, fid: usize) {
        // Java iterates the live effects list; propagation only creates
        // effects on OTHER entities, so the list can't change under us —
        // clone for the borrow checker.
        let effect_list = self.fighters[fid].effects.clone();
        for ei in effect_list {
            let (propagate, item_id, attack_type, effect_caster, critical) = {
                let e = &self.effects[ei];
                (e.propagate, e.item_id, e.attack_type, e.caster, e.critical)
            };
            if propagate <= 0 {
                continue;
            }
            // First effect line of the attack is the propagation information,
            // the second is the actual effect (`attack.getEffects().get(0/1)`).
            let spec_effects = match attack_type {
                AttackType::Weapon => self.weapon_specs[&item_id].effects.clone(),
                AttackType::Chip => self.chip_specs[&item_id].effects.clone(),
            };
            let propagation = spec_effects[0].clone();
            let original = spec_effects[1].clone();
            let stackable = propagation.modifiers.contains(EffectModifiers::STACKABLE);
            let jet = self.rng.get_double();
            // `getEntitiesAround(distance)` — every other entity within
            // `distance` cases (dead entities are 999 away). The Java
            // `HashMap<Integer, Entity>` iterates ascending for small fids.
            for target in 0..self.fighters.len() {
                if target == fid {
                    continue;
                }
                let dist = match (self.fighters[fid].cell, self.fighters[target].cell) {
                    (Some(a), Some(b))
                        if !self.fighters[fid].is_dead() && !self.fighters[target].is_dead() =>
                    {
                        self.map.get_cell_distance(a, b)
                    }
                    _ => 999,
                };
                if dist > propagate {
                    continue;
                }
                // La cible a déjà l'effet et il n'est pas remplaçable.
                if propagation
                    .modifiers
                    .contains(EffectModifiers::NOT_REPLACEABLE)
                    && self.has_effect(target, item_id)
                {
                    continue;
                }
                self.create_effect(
                    &original,
                    1.0,
                    critical,
                    target,
                    effect_caster,
                    item_id,
                    attack_type,
                    jet,
                    stackable,
                    0,
                    0,
                    propagate,
                );
            }
        }
    }

    /// `Effect.createEffect(...)` — build one effect instance, run the
    /// remove-previous / apply / stack-merge / store pipeline. Returns the
    /// effect's computed `value`.
    #[allow(clippy::too_many_arguments, clippy::cast_possible_wrap)]
    fn create_effect(
        &mut self,
        params: &EffectParams,
        aoe: f64,
        critical: bool,
        target: usize,
        caster: usize,
        item_id: i32,
        attack_type: AttackType,
        jet: f64,
        stackable: bool,
        previous_effect_total_value: i32,
        target_count: i32,
        propagate: i32,
    ) -> i32 {
        let critical_power = if critical { CRITICAL_FACTOR } else { 1.0 };
        let mut erosion_rate = if params.effect == EffectType::Poison {
            EROSION_POISON
        } else {
            EROSION_DAMAGE
        };
        if critical {
            erosion_rate += EROSION_CRITICAL_BONUS;
        }

        // Remove the previous effect of the same type (when not stackable).
        if params.turns != 0 && !stackable {
            let previous = self.fighters[target].effects.iter().copied().find(|&ei| {
                let e = &self.effects[ei];
                e.effect == params.effect && e.item_id == item_id
            });
            if let Some(ei) = previous {
                let prev_caster = self.effects[ei].caster;
                self.fighters[prev_caster]
                    .launched_effects
                    .retain(|&i| i != ei);
                self.remove_effect(target, ei);
            }
        }

        // Compute the effect (`effect.apply(state)`).
        let mut inst = EffectInstance {
            effect: params.effect,
            item_id,
            attack_type,
            turns: params.turns,
            value: 0,
            erosion_rate,
            critical,
            caster,
            target,
            stats: Stats::default(),
            log_id: 0,
            modifiers: params.modifiers,
            propagate,
            state: None,
        };
        match params.effect {
            EffectType::Damage => {
                inst.value = self.apply_effect_damage(
                    caster,
                    target,
                    aoe,
                    params.value1,
                    params.value2,
                    jet,
                    critical_power,
                    erosion_rate,
                    target_count,
                );
            }
            EffectType::Poison => self.apply_effect_poison(&mut inst, params, aoe, jet),
            EffectType::Heal => self.apply_effect_heal(&mut inst, params, aoe, jet, target_count),
            // The buff/shackle family all share `EffectBuffStrength`'s shape:
            // one scaling stat, one carried stat, value > 0 gate.
            EffectType::BuffStrength => {
                let science = self.fighters[caster].stat(STAT_SCIENCE);
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    science,
                    STAT_STRENGTH,
                    1,
                );
            }
            EffectType::AbsoluteShield => {
                let resistance = self.fighters[caster].stat(STAT_RESISTANCE);
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    resistance,
                    STAT_ABSOLUTE_SHIELD,
                    1,
                );
            }
            EffectType::RelativeShield => {
                let resistance = self.fighters[caster].stat(STAT_RESISTANCE);
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    resistance,
                    STAT_RELATIVE_SHIELD,
                    1,
                );
            }
            EffectType::DamageReturn => {
                let agility = self.fighters[caster].stat(STAT_AGILITY);
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    agility,
                    STAT_DAMAGE_RETURN,
                    1,
                );
            }
            EffectType::BuffAgility => {
                let science = self.fighters[caster].stat(STAT_SCIENCE);
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    science,
                    STAT_AGILITY,
                    1,
                );
            }
            EffectType::BuffMp => {
                let science = self.fighters[caster].stat(STAT_SCIENCE);
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, science, STAT_MP, 1);
            }
            EffectType::BuffTp => {
                let science = self.fighters[caster].stat(STAT_SCIENCE);
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, science, STAT_TP, 1);
            }
            EffectType::BuffWisdom => {
                let science = self.fighters[caster].stat(STAT_SCIENCE);
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    science,
                    STAT_WISDOM,
                    1,
                );
            }
            EffectType::BuffResistance => {
                let science = self.fighters[caster].stat(STAT_SCIENCE);
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    science,
                    STAT_RESISTANCE,
                    1,
                );
            }
            // The shackles carry a *negative* stat (scaled by max(0, magic)).
            EffectType::ShackleMp => {
                let magic = self.fighters[caster].stat(STAT_MAGIC).max(0);
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, magic, STAT_MP, -1);
            }
            EffectType::ShackleTp => {
                let magic = self.fighters[caster].stat(STAT_MAGIC).max(0);
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, magic, STAT_TP, -1);
            }
            EffectType::ShackleStrength => {
                let magic = self.fighters[caster].stat(STAT_MAGIC).max(0);
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    magic,
                    STAT_STRENGTH,
                    -1,
                );
            }
            EffectType::ShackleAgility => {
                let magic = self.fighters[caster].stat(STAT_MAGIC).max(0);
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    magic,
                    STAT_AGILITY,
                    -1,
                );
            }
            EffectType::ShackleWisdom => {
                let magic = self.fighters[caster].stat(STAT_MAGIC).max(0);
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, magic, STAT_WISDOM, -1);
            }
            EffectType::ShackleMagic => {
                let magic = self.fighters[caster].stat(STAT_MAGIC).max(0);
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, magic, STAT_MAGIC, -1);
            }
            EffectType::Vitality => self.apply_effect_vitality(&mut inst, params, aoe, jet),
            // `EffectDebuff.apply` — a *percentage*: truncating `(int)` cast
            // (NOT Math.round), no stat scaling, ×targetCount; reduce the
            // target's effects, then log `[306, fid, value]`.
            EffectType::Debuff => {
                #[allow(clippy::cast_possible_truncation)]
                {
                    inst.value = ((params.value1 + jet * params.value2)
                        * aoe
                        * critical_power
                        * f64::from(target_count)) as i32;
                }
                self.reduce_effects(target, f64::from(inst.value) / 100.0);
                self.actions.log(Action::ReduceEffects {
                    entity_id: target as i64,
                    value: inst.value,
                });
            }
            // `EffectAntidote.apply` — remove every poison (each removal logs
            // `[303]`), then log `[307, fid]`. The value stays 0, so nothing
            // ever merges or stores.
            EffectType::Antidote => {
                self.clear_poisons(target);
                self.actions.log(Action::RemovePoisons {
                    entity_id: target as i64,
                });
            }
            // `EffectRemoveShackles.apply` — remove every shackle effect
            // (each removal logs `[303]`), then log `[308, fid]`.
            EffectType::RemoveShackles => {
                self.remove_shackles(target);
                self.actions.log(Action::RemoveShackles {
                    entity_id: target as i64,
                });
            }
            // `EffectAddState.apply` — `value1` is the `EntityState` ordinal:
            // pin it on the target immediately AND carry it on the stored
            // effect (the state set is rebuilt from live effects, so expiry/
            // removal drops it).
            EffectType::AddState => {
                #[allow(clippy::cast_possible_truncation)]
                {
                    inst.value = params.value1 as i32;
                }
                let state = EntityState::from_ordinal(inst.value);
                inst.state = Some(state);
                self.fighters[target].states.push(state);
            }
            // `EffectPermutation.apply` — swap the caster with the target
            // (occupancy-only, no log; value stays 0, nothing stores).
            EffectType::Permutation => self.invert_entities(caster, target),
            // The slides already happened in `apply_on_cell`'s pre-block;
            // these keep the base no-op `Effect.apply` (value stays 0, so
            // nothing merges or stores).
            EffectType::Attract | EffectType::Push => {}
            // `EffectRepel` is an EMPTY class in the reference generator —
            // base no-op `apply`, no `Attack.java` pre-block either (unlike
            // attract/push). A repel line is dead: it moves nothing, logs
            // nothing, stores nothing.
            EffectType::Repel => {}
            // `EffectAllyKilledToAgility` is another EMPTY class — dead.
            EffectType::AllyKilledToAgility => {}
            // `EffectAftereffect.apply` — science-scaled damage that ALSO
            // ticks every turn (see `apply_start_turn_effect`). Logs
            // `DamageType.AFTEREFFECT` (the POISON wire id, 110). Unlike
            // direct damage there are NO shields, no life steal and no
            // return damage — but the INVINCIBLE zero and the action log
            // (even at 0) are kept.
            EffectType::Aftereffect => {
                let science = self.fighters[caster].stat(STAT_SCIENCE);
                inst.value = java_round(
                    (params.value1 + params.value2 * jet)
                        * (1.0 + f64::from(science) / 100.0)
                        * aoe
                        * critical_power,
                )
                .max(0);
                if self.fighters[target].has_state(EntityState::Invincible) {
                    inst.value = 0;
                }
                inst.value = inst.value.min(self.fighters[target].life);
                let erosion = java_round(f64::from(inst.value) * erosion_rate);
                self.actions.log(Action::Damage {
                    damage_type: DamageType::Poison,
                    target_id: target as i64,
                    pv: inst.value,
                    erosion,
                });
                self.remove_life(target, inst.value, erosion, Some(caster));
            }
            // `EffectKill.apply` — the INVINCIBLE guard is COMMENTED OUT in
            // the reference ("// Graal"), so kill pierces invincibility.
            // `ActionKill`'s constructor stores the TARGET fid in BOTH
            // fields (verbatim upstream bug).
            EffectType::Kill => {
                inst.value = self.fighters[target].life;
                self.actions.log(Action::Kill {
                    caster_id: target as i64,
                    target_id: target as i64,
                });
                self.remove_life(target, inst.value, 0, Some(caster));
            }
            EffectType::LifeDamage => {
                inst.value = self.apply_effect_life_damage(
                    caster,
                    target,
                    aoe,
                    params,
                    jet,
                    critical_power,
                    erosion_rate,
                );
            }
            // `EffectNovaDamage.apply` — science-scaled PURE EROSION: the
            // value is clamped to the missing life and removed from the max
            // life only (`removeLife(0, value)`), logged as `[107, fid,
            // value, 0]`.
            EffectType::NovaDamage => {
                let c = &self.fighters[caster];
                let mut d = (params.value1 + jet * params.value2)
                    * (1.0 + f64::from(c.stat(STAT_SCIENCE).max(0)) / 100.0)
                    * aoe
                    * critical_power
                    * (1.0 + f64::from(c.stat(STAT_POWER)) / 100.0);
                if self.fighters[target].has_state(EntityState::Invincible) {
                    d = 0.0;
                }
                let t = &self.fighters[target];
                inst.value = java_round(d).min(t.total_life - t.life);
                self.actions.log(Action::Damage {
                    damage_type: DamageType::Nova,
                    target_id: target as i64,
                    pv: inst.value,
                    erosion: 0,
                });
                self.remove_life(target, 0, inst.value, Some(caster));
            }
            // `EffectNovaVitality.apply` — science-scaled max-life bump with
            // NO floor at 0, NO invincible check and NO heal (unlike
            // `EffectVitality`): `addTotalLife` only moves `mTotalLife`.
            EffectType::NovaVitality => {
                let science = self.fighters[caster].stat(STAT_SCIENCE);
                inst.value = java_round(
                    (params.value1 + jet * params.value2)
                        * (1.0 + f64::from(science) / 100.0)
                        * aoe
                        * critical_power,
                );
                self.actions.log(Action::NovaVitality {
                    target_id: target as i64,
                    life: inst.value,
                });
                self.fighters[target].total_life += inst.value;
            }
            // The vulnerabilities are unscaled NEGATIVE shield carriers
            // (`(v1 + v2*jet) * aoe * critPower`, no caster stat — scaling 0
            // reuses the carrier shape).
            EffectType::Vulnerability => {
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    0,
                    STAT_RELATIVE_SHIELD,
                    -1,
                );
            }
            EffectType::AbsoluteVulnerability => {
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    0,
                    STAT_ABSOLUTE_SHIELD,
                    -1,
                );
            }
            // The raw buff family: same carrier shape with NO scaling stat.
            EffectType::RawBuffStrength => {
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, 0, STAT_STRENGTH, 1);
            }
            EffectType::RawBuffAgility => {
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, 0, STAT_AGILITY, 1);
            }
            EffectType::RawBuffMagic => {
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, 0, STAT_MAGIC, 1);
            }
            EffectType::RawBuffScience => {
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, 0, STAT_SCIENCE, 1);
            }
            EffectType::RawBuffWisdom => {
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, 0, STAT_WISDOM, 1);
            }
            EffectType::RawBuffResistance => {
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, 0, STAT_RESISTANCE, 1);
            }
            EffectType::RawBuffPower => {
                self.apply_effect_stat_carrier(&mut inst, params, aoe, jet, 0, STAT_POWER, 1);
            }
            EffectType::RawAbsoluteShield => {
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    0,
                    STAT_ABSOLUTE_SHIELD,
                    1,
                );
            }
            EffectType::RawRelativeShield => {
                self.apply_effect_stat_carrier(
                    &mut inst,
                    params,
                    aoe,
                    jet,
                    0,
                    STAT_RELATIVE_SHIELD,
                    1,
                );
            }
            // `EffectRawBuffMP` / `EffectRawBuffTP` break the carrier shape:
            // `(v1 + v2*jet) * targetCount * critPower` — targetCount
            // REPLACES the aoe factor.
            EffectType::RawBuffMp | EffectType::RawBuffTp => {
                inst.value = java_round(
                    (params.value1 + params.value2 * jet)
                        * f64::from(target_count)
                        * critical_power,
                );
                if inst.value > 0 {
                    let stat = if params.effect == EffectType::RawBuffMp {
                        STAT_MP
                    } else {
                        STAT_TP
                    };
                    inst.stats.set(stat, inst.value);
                    self.fighters[target].buff_stats.update(stat, inst.value);
                }
            }
            // `EffectRawHeal.apply` — an UNHEALABLE target returns before
            // anything; NO wisdom scaling, NO floor at 0, the missing-life
            // cap only bites from above; logs even a 0 heal.
            EffectType::RawHeal => {
                if !self.fighters[target].has_state(EntityState::Unhealable) {
                    inst.value = java_round(
                        (params.value1 + jet * params.value2)
                            * aoe
                            * critical_power
                            * f64::from(target_count),
                    );
                    let t = &self.fighters[target];
                    if t.life + inst.value > t.total_life {
                        inst.value = t.total_life - t.life;
                    }
                    self.actions.log(Action::Heal {
                        target_id: target as i64,
                        life: inst.value,
                    });
                    self.add_life(target, inst.value);
                }
            }
            // `EffectTotalDebuff.apply` — like `EffectDebuff` (truncating
            // `(int)` cast, ×targetCount) but reduces EVERY effect, even
            // IRREDUCTIBLE ones.
            EffectType::TotalDebuff => {
                #[allow(clippy::cast_possible_truncation)]
                {
                    inst.value = ((params.value1 + jet * params.value2)
                        * aoe
                        * critical_power
                        * f64::from(target_count)) as i32;
                }
                self.reduce_effects_total(target, f64::from(inst.value) / 100.0);
                self.actions.log(Action::ReduceEffects {
                    entity_id: target as i64,
                    value: inst.value,
                });
            }
            // `EffectStealLife.apply` — heal by the PREVIOUS effect line's
            // total value (the damage it dealt across its targets). The
            // UNHEALABLE return comes first; positive values cap and log
            // like a heal.
            EffectType::StealLife => {
                if !self.fighters[target].has_state(EntityState::Unhealable) {
                    inst.value = previous_effect_total_value;
                    if inst.value > 0 {
                        let t = &self.fighters[target];
                        if t.life + inst.value > t.total_life {
                            inst.value = t.total_life - t.life;
                        }
                        self.actions.log(Action::Heal {
                            target_id: target as i64,
                            life: inst.value,
                        });
                        self.add_life(target, inst.value);
                    }
                }
            }
            // `EffectStealAbsoluteShield.apply` — carry the previous effect
            // line's total value as absolute shield.
            EffectType::StealAbsoluteShield => {
                inst.value = previous_effect_total_value;
                if inst.value > 0 {
                    inst.stats.set(STAT_ABSOLUTE_SHIELD, inst.value);
                    self.fighters[target]
                        .buff_stats
                        .update(STAT_ABSOLUTE_SHIELD, inst.value);
                }
            }
            // `EffectMultiplyStats.apply` — the Colossus boost. factor =
            // `(int) value1` (truncating); ≤ 1 is a full no-op (`value`
            // stays 0, so nothing is stored). Every base stat gains
            // `base * (factor - 1)` carried on the effect; max life gains
            // `lifeBase * (factor - 1)` on first apply but exactly
            // `lifeBase` on a replacement (`removeEffect` never undoes
            // `addTotalLife`, so the old bonus is still in `mTotalLife`),
            // then life tops up to keep the ratio with NO heal action
            // (`addLife` is statistics-only there).
            EffectType::MultiplyStats => {
                #[allow(clippy::cast_possible_truncation)]
                let factor = params.value1 as i32;
                if factor > 1 {
                    inst.value = factor;
                    for stat in [
                        STAT_STRENGTH,
                        STAT_AGILITY,
                        STAT_RESISTANCE,
                        STAT_WISDOM,
                        STAT_SCIENCE,
                        STAT_MAGIC,
                        STAT_FREQUENCY,
                        STAT_TP,
                        STAT_MP,
                    ] {
                        let buff = self.fighters[target].base_stats.get(stat) * (factor - 1);
                        if buff > 0 {
                            inst.stats.set(stat, buff);
                            self.fighters[target].buff_stats.update(stat, buff);
                        }
                    }
                    let life_base = self.fighters[target].base_stats.get(STAT_LIFE);
                    let t = &mut self.fighters[target];
                    let life_delta = if t.total_life <= life_base {
                        life_base * (factor - 1)
                    } else {
                        life_base
                    };
                    let ratio = if t.total_life > 0 {
                        f64::from(t.life) / f64::from(t.total_life)
                    } else {
                        1.0
                    };
                    t.total_life += life_delta;
                    let heal = java_round(f64::from(t.total_life) * ratio) - t.life;
                    if heal > 0 {
                        self.add_life(target, heal);
                    }
                }
            }
            other => panic!("effect type {other:?} not ported yet (corpus-first)"),
        }

        // Stack onto a previous effect with the same characteristics.
        if inst.value > 0 {
            let same = self.fighters[target].effects.iter().copied().find(|&ei| {
                let e = &self.effects[ei];
                e.item_id == item_id
                    && e.effect == params.effect
                    && e.turns == params.turns
                    && e.caster == caster
            });
            if let Some(ei) = same {
                // `Effect.mergeWith` — bump the value, and add the new value
                // (signed like the carried stat) to every nonzero stat.
                let add = inst.value;
                let e = &mut self.effects[ei];
                e.value += add;
                for stat in 0..STAT_COUNT {
                    let v = e.stats.get(stat);
                    if v != 0 {
                        e.stats.update(stat, add * v.signum());
                    }
                }
                let log_id = e.log_id;
                self.actions.log(Action::StackEffect { log_id, value: add });
                return add; // No need to store the effect again.
            }
        }

        // Add the effect to the target and the caster.
        if params.turns != 0 && inst.value > 0 {
            let ei = self.effects.len();
            self.fighters[target].effects.push(ei);
            self.fighters[caster].launched_effects.push(ei);
            // `effect.addLog(state)`.
            inst.log_id = self.actions.create_effect(
                attack_type.action_type(),
                item_id,
                caster as i64,
                target as i64,
                params.effect.id(),
                inst.value,
                inst.turns,
                params.modifiers.bits(),
            );
            let value = inst.value;
            self.effects.push(inst);
            return value;
        }
        inst.value
    }

    /// `EffectPoison.apply(state)` — compute the per-turn poison value
    /// (magic-scaled; **no** target-count factor). The damage itself ticks in
    /// [`State::apply_start_turn_effect`].
    fn apply_effect_poison(
        &mut self,
        inst: &mut EffectInstance,
        params: &EffectParams,
        aoe: f64,
        jet: f64,
    ) {
        let c = &self.fighters[inst.caster];
        let critical_power = if inst.critical { CRITICAL_FACTOR } else { 1.0 };
        inst.value = java_round(
            (params.value1 + jet * params.value2)
                * (1.0 + f64::from(c.stat(STAT_MAGIC).max(0)) / 100.0)
                * aoe
                * critical_power
                * (1.0 + f64::from(c.stat(STAT_POWER)) / 100.0),
        );
    }

    /// The shared shape of `EffectBuffStrength` / `EffectAbsoluteShield` /
    /// `EffectRelativeShield` / `EffectShackleMP` / `EffectShackleTP` /
    /// `EffectDamageReturn` `.apply(state)`: compute the value from one
    /// scaling stat of the caster, then (when positive) carry it as `sign *
    /// value` on `stat` and bump the target's buff stats directly
    /// (`updateBuffStats(stat, sign * value, caster)`).
    #[allow(clippy::too_many_arguments)]
    fn apply_effect_stat_carrier(
        &mut self,
        inst: &mut EffectInstance,
        params: &EffectParams,
        aoe: f64,
        jet: f64,
        scaling: i32,
        stat: usize,
        sign: i32,
    ) {
        let critical_power = if inst.critical { CRITICAL_FACTOR } else { 1.0 };
        inst.value = java_round(
            (params.value1 + jet * params.value2)
                * (1.0 + f64::from(scaling) / 100.0)
                * aoe
                * critical_power,
        );
        if inst.value > 0 {
            inst.stats.set(stat, sign * inst.value);
            self.fighters[inst.target]
                .buff_stats
                .update(stat, sign * inst.value);
        }
    }

    /// `EffectHeal.apply(state)` — wisdom-scaled heal **with** the
    /// target-count factor (unlike poison), floored at 0 (negative wisdom).
    /// `turns == 0` is the instant heal: cap to the missing life, log
    /// `ActionHeal` (Java logs even a 0 heal) and add the life. `turns != 0`
    /// only computes the per-turn value here — the healing ticks in
    /// [`State::apply_start_turn_effect`].
    #[allow(clippy::cast_possible_wrap)]
    fn apply_effect_heal(
        &mut self,
        inst: &mut EffectInstance,
        params: &EffectParams,
        aoe: f64,
        jet: f64,
        target_count: i32,
    ) {
        let wisdom = self.fighters[inst.caster].stat(STAT_WISDOM);
        let critical_power = if inst.critical { CRITICAL_FACTOR } else { 1.0 };
        inst.value = java_round(
            (params.value1 + jet * params.value2)
                * (1.0 + f64::from(wisdom) / 100.0)
                * aoe
                * critical_power
                * f64::from(target_count),
        )
        .max(0);
        if inst.turns == 0 {
            // An UNHEALABLE target returns BEFORE the log — no 0-heal action
            // (unlike the full-life cap, which logs 0).
            if self.fighters[inst.target].has_state(EntityState::Unhealable) {
                return;
            }
            let t = &self.fighters[inst.target];
            inst.value = inst.value.min(t.total_life - t.life);
            self.actions.log(Action::Heal {
                target_id: inst.target as i64,
                life: inst.value,
            });
            self.add_life(inst.target, inst.value);
        }
    }

    /// `EffectVitality.apply(state)` — wisdom-scaled max-life increase,
    /// floored at 0 (negative wisdom). Logs `ActionVitality` UNCONDITIONALLY
    /// (even 0), bumps `totalLife` (a plain field in Java — the increase is
    /// PERMANENT, expiry never reverts it since the stored effect carries no
    /// stats) and heals by the same amount.
    #[allow(clippy::cast_possible_wrap)]
    fn apply_effect_vitality(
        &mut self,
        inst: &mut EffectInstance,
        params: &EffectParams,
        aoe: f64,
        jet: f64,
    ) {
        let wisdom = self.fighters[inst.caster].stat(STAT_WISDOM);
        let critical_power = if inst.critical { CRITICAL_FACTOR } else { 1.0 };
        inst.value = java_round(
            (params.value1 + jet * params.value2)
                * (1.0 + f64::from(wisdom) / 100.0)
                * aoe
                * critical_power,
        )
        .max(0);
        self.actions.log(Action::Vitality {
            target_id: inst.target as i64,
            life: inst.value,
        });
        self.fighters[inst.target].total_life += inst.value;
        self.add_life(inst.target, inst.value);
    }

    /// `EffectLifeDamage.apply(state)` — damage proportional to the CASTER's
    /// current life (`(v1 + jet*v2)/100 × caster.life`), power-scaled.
    /// Same shield/clamp/erosion/return tail as `EffectDamage`, except: the
    /// INVINCIBLE zero happens BEFORE the return damage is computed (so an
    /// invincible target reflects nothing), and there is NO life steal.
    #[allow(clippy::too_many_arguments, clippy::cast_possible_wrap)]
    fn apply_effect_life_damage(
        &mut self,
        caster: usize,
        target: usize,
        aoe: f64,
        params: &EffectParams,
        jet: f64,
        critical_power: f64,
        erosion_rate: f64,
    ) -> i32 {
        let c = &self.fighters[caster];
        let mut d = ((params.value1 + jet * params.value2) / 100.0)
            * f64::from(c.life)
            * aoe
            * critical_power
            * (1.0 + f64::from(c.stat(STAT_POWER)) / 100.0);

        if self.fighters[target].has_state(EntityState::Invincible) {
            d = 0.0;
        }

        // Return damage (the value is already zeroed for invincible targets).
        let t = &self.fighters[target];
        let mut return_damage = if target == caster {
            0
        } else {
            java_round(d * f64::from(t.stat(STAT_DAMAGE_RETURN)) / 100.0)
        };

        // Shields.
        d -= d * (f64::from(t.stat(STAT_RELATIVE_SHIELD)) / 100.0)
            + f64::from(t.stat(STAT_ABSOLUTE_SHIELD));
        d = d.max(0.0);

        let mut value = java_round(d);
        if t.life < value {
            value = t.life;
        }
        let erosion = java_round(f64::from(value) * erosion_rate);

        self.actions.log(Action::Damage {
            damage_type: DamageType::Life,
            target_id: target as i64,
            pv: value,
            erosion,
        });
        self.remove_life(target, value, erosion, Some(caster));

        // Return damage — an INVINCIBLE caster takes none back.
        if return_damage > 0 && !self.fighters[caster].has_state(EntityState::Invincible) {
            if self.fighters[caster].life < return_damage {
                return_damage = self.fighters[caster].life;
            }
            let return_erosion = java_round(f64::from(return_damage) * erosion_rate);
            if return_damage > 0 {
                self.actions.log(Action::Damage {
                    damage_type: DamageType::Return,
                    target_id: caster as i64,
                    pv: return_damage,
                    erosion: return_erosion,
                });
                self.remove_life(caster, return_damage, return_erosion, Some(target));
            }
        }
        value
    }

    /// `EffectDamage.apply(state)` — the full direct-damage pipeline:
    /// strength/power scaling, shields, life clamp, erosion, life steal
    /// (wisdom), and damage return.
    #[allow(clippy::too_many_arguments, clippy::cast_possible_wrap)]
    fn apply_effect_damage(
        &mut self,
        caster: usize,
        target: usize,
        aoe: f64,
        value1: f64,
        value2: f64,
        jet: f64,
        critical_power: f64,
        erosion_rate: f64,
        target_count: i32,
    ) -> i32 {
        // Base damage.
        let c = &self.fighters[caster];
        let mut d = (value1 + jet * value2)
            * (1.0 + f64::from(c.stat(STAT_STRENGTH).max(0)) / 100.0)
            * aoe
            * critical_power
            * f64::from(target_count)
            * (1.0 + f64::from(c.stat(STAT_POWER)) / 100.0);

        // Return damage (from the pre-shield value).
        let t = &self.fighters[target];
        let mut return_damage = if target == caster {
            0
        } else {
            java_round(d * f64::from(t.stat(STAT_DAMAGE_RETURN)) / 100.0)
        };

        // Shields, then INVINCIBLE zeroes the damage — but the return damage
        // above was already taken from the raw pre-shield value, and the
        // 0-damage action still logs below.
        d -= d * (f64::from(t.stat(STAT_RELATIVE_SHIELD)) / 100.0)
            + f64::from(t.stat(STAT_ABSOLUTE_SHIELD));
        d = d.max(0.0);
        if t.has_state(EntityState::Invincible) {
            d = 0.0;
        }

        let mut value = java_round(d);
        if t.life < value {
            value = t.life;
        }

        // Life steal (from the post-clamp value).
        let mut life_steal = if target == caster {
            0
        } else {
            java_round(
                f64::from(value) * f64::from(self.fighters[caster].stat(STAT_WISDOM)) / 1000.0,
            )
        };

        let erosion = java_round(f64::from(value) * erosion_rate);

        self.actions.log(Action::Damage {
            damage_type: DamageType::Direct,
            target_id: target as i64,
            pv: value,
            erosion,
        });
        self.remove_life(target, value, erosion, Some(caster));
        // onDirectDamage / onNovaDamage: weapon passive effects — none in
        // the leek scope yet.

        // Life steal — an UNHEALABLE caster steals nothing.
        if !self.fighters[caster].is_dead()
            && life_steal > 0
            && self.fighters[caster].life < self.fighters[caster].total_life
            && !self.fighters[caster].has_state(EntityState::Unhealable)
        {
            let cl = &self.fighters[caster];
            if cl.life + life_steal > cl.total_life {
                life_steal = cl.total_life - cl.life;
            }
            if life_steal > 0 {
                self.actions.log(Action::Heal {
                    target_id: caster as i64,
                    life: life_steal,
                });
                self.add_life(caster, life_steal);
            }
        }

        // Return damage — an INVINCIBLE caster takes none back.
        if return_damage > 0 && !self.fighters[caster].has_state(EntityState::Invincible) {
            if self.fighters[caster].life < return_damage {
                return_damage = self.fighters[caster].life;
            }
            let return_erosion = java_round(f64::from(return_damage) * erosion_rate);
            if return_damage > 0 {
                self.actions.log(Action::Damage {
                    damage_type: DamageType::Return,
                    target_id: caster as i64,
                    pv: return_damage,
                    erosion: return_erosion,
                });
                self.remove_life(caster, return_damage, return_erosion, Some(target));
            }
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::{Area, EffectType, java_round};

    #[test]
    fn java_round_matches_math_round() {
        // JDK-8010430: floor(x + 0.5) would give 1 here; Math.round gives 0.
        assert_eq!(java_round(0.499_999_999_999_999_94), 0);
        assert_eq!(java_round(0.5), 1);
        assert_eq!(java_round(1.5), 2);
        assert_eq!(java_round(-0.5), 0); // ties toward +∞
        assert_eq!(java_round(-1.5), -1);
        assert_eq!(java_round(-1.6), -2);
        assert_eq!(java_round(2.4), 2);
    }

    #[test]
    fn effect_type_ids() {
        // from_id must be the inverse of id() over the whole official range.
        for id in 1..=62 {
            let e = EffectType::from_id(id).expect("ids 1..=62 are all defined");
            assert_eq!(e.id(), id);
        }
        assert_eq!(EffectType::from_id(0), None);
        assert_eq!(EffectType::from_id(63), None);
    }

    #[test]
    fn area_ids() {
        for id in 1..=15 {
            let a = Area::from_id(id).expect("ids 1..=15 are all defined");
            assert_eq!(a.id(), id);
        }
        assert_eq!(Area::from_id(0), None);
        assert_eq!(Area::from_id(16), None);
    }
}
