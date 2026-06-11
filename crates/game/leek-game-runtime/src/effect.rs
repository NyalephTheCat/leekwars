//! The effect/stat model: buffable characteristics ([`Stat`]), the kinds of
//! effects weapons and chips apply ([`EffectKind`]), a catalog effect roll
//! ([`Effect`]), and an effect instance active on an entity ([`ActiveEffect`]).

/// A buffable combat characteristic. `Mp`/`Tp` buffs raise the per-turn
/// point pools (and grant the points immediately when applied).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stat {
    Strength,
    Agility,
    Wisdom,
    Resistance,
    Science,
    Magic,
    Power,
    Mp,
    Tp,
}

/// A kind of effect a weapon or chip applies. The scaling stat and whether
/// it's instant or lasts `turns` follow from the kind (see [`apply_effect`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    /// Instant damage, scaled by the caster's strength (+ crit, power,
    /// life-steal, erosion, damage-return).
    Damage,
    /// Instant heal, scaled by wisdom.
    Heal,
    /// Flat damage shield for `turns`, scaled by resistance.
    AbsoluteShield,
    /// Percent damage shield for `turns`.
    RelativeShield,
    /// A stat buff for `turns`, scaled by science. Stored with a signed value
    /// (a [`EffectKind::Shackle`] stores a negative one).
    Buff(Stat),
    /// A stat buff for `turns`, **unscaled** (upstream `TYPE_RAW_BUFF_*`).
    /// Applied as a [`EffectKind::Buff`] active effect.
    RawBuff(Stat),
    /// Flat damage shield for `turns`, **unscaled** (no resistance bonus).
    /// Applied as a [`EffectKind::AbsoluteShield`] active effect.
    RawAbsoluteShield,
    /// Percent damage shield for `turns`, **unscaled**.
    /// Applied as a [`EffectKind::RelativeShield`] active effect.
    RawRelativeShield,
    /// Instant heal, **unscaled** (no wisdom bonus).
    RawHeal,
    /// Instant damage as `value%` of the **caster's** life, scaled by power
    /// (not strength). No life steal; shields and damage-return apply.
    LifeDamage,
    /// Set the target's life to 0. Instant.
    Kill,
    /// Damage-return buff for `turns`, scaled by agility.
    DamageReturn,
    /// Grant an absolute shield equal to the **previous effect's** total
    /// applied value (upstream `previousEffectTotalValue` chaining; usually
    /// paired with [`MODIFIER_ON_CASTER`]). The rolled value is ignored.
    StealAbsoluteShield,
    /// Raise max life only (no heal), scaled by science. Instant.
    NovaVitality,
    /// Reduce every active effect on the target by the rolled percent, except
    /// effects carrying [`MODIFIER_IRREDUCTIBLE`]. Instant.
    Debuff,
    /// Like [`EffectKind::Debuff`], but reduces irreductible effects too
    /// (upstream TOTAL_DEBUFF). Instant.
    TotalDebuff,
    /// Remove all shackles (negative stat buffs) from the target. Instant.
    RemoveShackles,
    /// Per-turn damage for `turns`, scaled by magic.
    Poison,
    /// Per-turn heal for `turns`, scaled by wisdom.
    Regeneration,
    /// Max-life damage (erosion), scaled by science. Instant.
    Nova,
    /// A stat debuff for `turns`, scaled by the caster's magic (stored as a
    /// negative [`EffectKind::Buff`]).
    Shackle(Stat),
    /// Negative shield for `turns` — increases damage taken. `absolute` flat
    /// vs. percent (stored as a negative shield).
    Vulnerability { absolute: bool },
    /// Raise max life and heal by the same amount, scaled by wisdom. Instant.
    Vitality,
    /// Remove all poison from the target. Instant.
    Antidote,
    /// Revive a dead target: its max life is halved (min 10) and it comes
    /// back at half the new max. Instant.
    Resurrect,
    /// An effect type the engine doesn't model yet, carrying the upstream
    /// generator's effect-type id (`Effect.TYPE_*`). Applying it is a no-op.
    Unsupported(u8),
}

impl EffectKind {
    /// Map an upstream generator effect-type id (`Effect.TYPE_*` in the
    /// official `Effect.java`, the `id` field of `effects` entries in
    /// `data/weapons.json` / `data/chips.json`) to the engine's kind.
    /// Unmodeled ids become [`EffectKind::Unsupported`].
    #[must_use]
    pub const fn from_upstream(id: u8) -> Self {
        match id {
            1 => Self::Damage,
            // Upstream heal-over-time is TYPE_HEAL with `turns > 0`; the
            // apply step turns those into a Regeneration active effect.
            2 => Self::Heal,
            3 => Self::Buff(Stat::Strength),
            4 => Self::Buff(Stat::Agility),
            5 => Self::RelativeShield,
            6 => Self::AbsoluteShield,
            7 => Self::Buff(Stat::Mp),
            8 => Self::Buff(Stat::Tp),
            9 => Self::Debuff,
            60 => Self::TotalDebuff,
            12 => Self::Vitality,
            13 => Self::Poison,
            15 => Self::Resurrect,
            16 => Self::Kill,
            17 => Self::Shackle(Stat::Mp),
            18 => Self::Shackle(Stat::Tp),
            19 => Self::Shackle(Stat::Strength),
            20 => Self::DamageReturn,
            21 => Self::Buff(Stat::Resistance),
            22 => Self::Buff(Stat::Wisdom),
            23 => Self::Antidote,
            24 => Self::Shackle(Stat::Magic),
            26 => Self::Vulnerability { absolute: false },
            27 => Self::Vulnerability { absolute: true },
            28 => Self::LifeDamage,
            29 => Self::StealAbsoluteShield,
            30 => Self::Nova,
            31 => Self::RawBuff(Stat::Mp),
            32 => Self::RawBuff(Stat::Tp),
            37 => Self::RawAbsoluteShield,
            38 => Self::RawBuff(Stat::Strength),
            39 => Self::RawBuff(Stat::Magic),
            40 => Self::RawBuff(Stat::Science),
            41 => Self::RawBuff(Stat::Agility),
            42 => Self::RawBuff(Stat::Resistance),
            44 => Self::RawBuff(Stat::Wisdom),
            45 => Self::NovaVitality,
            47 => Self::Shackle(Stat::Agility),
            48 => Self::Shackle(Stat::Wisdom),
            49 => Self::RemoveShackles,
            52 => Self::RawBuff(Stat::Power),
            54 => Self::RawRelativeShield,
            57 => Self::RawHeal,
            other => Self::Unsupported(other),
        }
    }
}

/// Bits of the upstream target mask: which entities in the area an effect
/// touches. The caster must also pass the ally check — an effect that targets
/// the caster but not allies never reaches it (as upstream).
pub const TARGET_ENEMIES: u8 = 1;
pub const TARGET_ALLIES: u8 = 2;
pub const TARGET_CASTER: u8 = 4;
/// Entities that are not summons. The engine doesn't model summons, so every
/// entity is a non-summon: a mask without this bit matches nobody.
pub const TARGET_NON_SUMMONS: u8 = 8;
pub const TARGET_SUMMONS: u8 = 16;

/// Everyone in the area is affected — all bits of the upstream target mask.
pub const TARGET_ALL: u8 =
    TARGET_ENEMIES | TARGET_ALLIES | TARGET_CASTER | TARGET_NON_SUMMONS | TARGET_SUMMONS;

/// Upstream `MODIFIER_STACKABLE`: re-applying the effect stacks with the
/// previous instance instead of replacing it. Casts with identical
/// (item, kind, remaining turns, caster) merge into one entry either way.
pub const MODIFIER_STACKABLE: u8 = 1;
/// Upstream `MODIFIER_MULTIPLIED_BY_TARGETS`: the effect's value is
/// multiplied by the number of entities it targets (only the upstream
/// formulas that consume `targetCount` honor it: damage, heals, raw MP/TP
/// buffs, and debuffs).
pub const MODIFIER_MULTIPLIED_BY_TARGETS: u8 = 2;
/// Upstream `MODIFIER_ON_CASTER`: the effect applies once to the caster
/// instead of the entities in the area (which are still counted for
/// [`MODIFIER_MULTIPLIED_BY_TARGETS`]).
pub const MODIFIER_ON_CASTER: u8 = 4;
/// Upstream `MODIFIER_NOT_REPLACEABLE`: the effect is skipped on targets
/// that already carry any effect from the same item.
pub const MODIFIER_NOT_REPLACEABLE: u8 = 8;
/// Upstream `MODIFIER_IRREDUCTIBLE`: the effect survives [`EffectKind::Debuff`]
/// (but not [`EffectKind::TotalDebuff`]).
pub const MODIFIER_IRREDUCTIBLE: u8 = 16;

/// One effect of a weapon/chip: a `value1 + jet·value2` roll applied as
/// [`EffectKind`], lasting `turns` (0 = instant). Values are fractional for
/// some upstream effects (e.g. MP/TP shackles roll `0.3 + jet·0.1`).
#[derive(Debug, Clone, Copy)]
pub struct Effect {
    pub kind: EffectKind,
    pub value1: f64,
    pub value2: f64,
    pub turns: i64,
    /// Upstream target mask — which entities in the area the effect touches
    /// (see the `TARGET_*` bits).
    pub targets: u8,
    /// Upstream modifier bits (see [`MODIFIER_ON_CASTER`]).
    pub modifiers: u8,
}

impl Effect {
    /// Convenience constructor (affects everyone in the area, no modifiers).
    #[must_use]
    pub const fn new(kind: EffectKind, value1: f64, value2: f64, turns: i64) -> Self {
        Self {
            kind,
            value1,
            value2,
            turns,
            targets: TARGET_ALL,
            modifiers: 0,
        }
    }

    /// An effect as the upstream catalogs encode it: the generator's
    /// effect-type `id` plus the raw roll/duration/target/modifier fields.
    /// Used by the generated weapon/chip catalogs.
    #[must_use]
    pub const fn from_upstream(
        id: u8,
        value1: f64,
        value2: f64,
        turns: i64,
        targets: u8,
        modifiers: u8,
    ) -> Self {
        Self {
            kind: EffectKind::from_upstream(id),
            value1,
            value2,
            turns,
            targets,
            modifiers,
        }
    }
}

/// The upstream effect-type ids in `effects` the engine doesn't model
/// (empty = fully supported). Used by strict-mode scenario validation.
#[must_use]
pub fn unsupported_ids(effects: &[Effect]) -> Vec<u8> {
    effects
        .iter()
        .filter_map(|e| match e.kind {
            EffectKind::Unsupported(id) => Some(id),
            _ => None,
        })
        .collect()
}

/// A lasting effect on an entity (shield / buff / poison) with a remaining
/// duration in turns. `item`/`caster`/`modifiers` identify where it came
/// from: replacement and stacking key on them (upstream `createEffect`), and
/// [`MODIFIER_IRREDUCTIBLE`] shields it from debuffs.
#[derive(Debug, Clone, Copy)]
pub struct ActiveEffect {
    pub kind: EffectKind,
    pub value: i64,
    pub turns: i64,
    /// The weapon/chip that applied it (0 = none, e.g. scenario-injected).
    pub item: i64,
    /// The entity that cast it (0 = none).
    pub caster: i64,
    /// The upstream modifier bits of the catalog effect that applied it.
    pub modifiers: u8,
}

impl ActiveEffect {
    /// An effect with no originating item or caster (scenario-injected or
    /// test fixture) and no modifiers.
    #[must_use]
    pub const fn injected(kind: EffectKind, value: i64, turns: i64) -> Self {
        Self {
            kind,
            value,
            turns,
            item: 0,
            caster: 0,
            modifiers: 0,
        }
    }
}
