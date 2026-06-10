//! The effect/stat model: buffable characteristics ([`Stat`]), the kinds of
//! effects weapons and chips apply ([`EffectKind`]), a catalog effect roll
//! ([`Effect`]), and an effect instance active on an entity ([`ActiveEffect`]).

/// A buffable combat characteristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stat {
    Strength,
    Agility,
    Wisdom,
    Resistance,
    Science,
    Magic,
    Power,
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
    /// Revive a dead target to `value` life, scaled by wisdom. Instant.
    Resurrect,
}

/// One effect of a weapon/chip: a `value1 + jet·value2` roll applied as
/// [`EffectKind`], lasting `turns` (0 = instant).
#[derive(Debug, Clone, Copy)]
pub struct Effect {
    pub kind: EffectKind,
    pub value1: i64,
    pub value2: i64,
    pub turns: i64,
}

impl Effect {
    /// Convenience constructor.
    #[must_use]
    pub const fn new(kind: EffectKind, value1: i64, value2: i64, turns: i64) -> Self {
        Self {
            kind,
            value1,
            value2,
            turns,
        }
    }
}

/// A lasting effect on an entity (shield / buff / poison) with a remaining
/// duration in turns.
#[derive(Debug, Clone, Copy)]
pub struct ActiveEffect {
    pub kind: EffectKind,
    pub value: i64,
    pub turns: i64,
}
