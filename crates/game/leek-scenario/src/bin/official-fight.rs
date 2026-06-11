//! `official-fight` — the Rust twin of `tools/fight-harness/fight.sh`.
//!
//! Runs a 1v1 between two compiled `.leek` AIs through the official-parity
//! engine ([`leek_generator::official`]) under the exact fight-harness
//! scenario (`tools/fight-harness/Harness.java`): two stock level-10 leeks
//! and the synthetic pistol 37. Prints the official Outcome JSON on stdout —
//! byte-comparable (modulo `ops`/`execution_time`) with the Java harness's
//! output for the same AIs and seed, which is what
//! `tools/fight-harness/check-conformance.sh` does.

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use leek_generator::official::{
    Area, BulbTemplate, ChipSpec, EffectModifiers, EffectParams, EffectTargets, EffectType,
    Fighter, STAT_AGILITY, STAT_FREQUENCY, STAT_LIFE, STAT_MP, STAT_RESISTANCE, STAT_STRENGTH,
    STAT_TP, STAT_WISDOM, State, Stats, WeaponSpec, run_official_fight,
};

/// `Harness.defaultLeek` — the stock fight-harness leek with the synthetic
/// pistol (id 37) in its inventory.
fn harness_leek(id: i64, name: &str) -> Fighter {
    let mut stats = Stats::default();
    stats.set(STAT_LIFE, 500);
    stats.set(STAT_TP, 6);
    stats.set(STAT_MP, 7);
    stats.set(STAT_STRENGTH, 100);
    stats.set(STAT_AGILITY, 100);
    stats.set(STAT_FREQUENCY, 10);
    stats.set(STAT_WISDOM, 50);
    stats.set(STAT_RESISTANCE, 10);
    let mut f = Fighter::new(0, id, name.to_string(), 0, stats);
    f.level = 10;
    f.weapons = vec![37];
    f.chips = (1001..=1049).collect();
    f.chips.insert(84); // CHIP_RESURRECTION — exactly 50/50 RAM
    f
}

/// `Harness.registerPistol` — synthetic pistol 37, one instant-damage effect
/// (15 + 5×jet).
fn harness_pistol() -> WeaponSpec {
    WeaponSpec {
        id: 37,
        cost: 3,
        min_range: 1,
        max_range: 7,
        launch_type: 1,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Damage,
            value1: 15.0,
            value2: 5.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
    }
}

/// `Harness.registerChips` — synthetic chip 1001 "venom": poison 10 + 5×jet
/// for 2 turns, with an entity cooldown of 2 and an initial cooldown of 1.
fn harness_venom() -> ChipSpec {
    ChipSpec {
        id: 1001,
        cost: 2,
        min_range: 1,
        max_range: 7,
        launch_type: 1,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Poison,
            value1: 10.0,
            value2: 5.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 2,
        team_cooldown: false,
        initial_cooldown: 1,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1002 "protein": stackable
/// strength buff 5 + 5×jet for 3 turns, free (0 TP), self-only (range 0).
fn harness_protein() -> ChipSpec {
    ChipSpec {
        id: 1002,
        cost: 0,
        min_range: 0,
        max_range: 0,
        launch_type: 1,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::BuffStrength,
            value1: 5.0,
            value2: 5.0,
            turns: 3,
            targets: EffectTargets::ALLIES | EffectTargets::CASTER | EffectTargets::NON_SUMMONS,
            modifiers: EffectModifiers::STACKABLE,
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1003 "magnet": self-cast (range
/// 0) attract over CIRCLE_3, pulling every entity within 3 cells straight to
/// the caster.
fn harness_magnet() -> ChipSpec {
    ChipSpec {
        id: 1003,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::Circle3,
        effects: vec![EffectParams {
            effect: EffectType::Attract,
            value1: 0.0,
            value2: 0.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1004 "glove": push + splash
/// damage (10, no jet) over CIRCLE_2, no LoS — cast at the cell behind the
/// enemy so the push direction lines up.
fn harness_glove() -> ChipSpec {
    ChipSpec {
        id: 1004,
        cost: 2,
        min_range: 1,
        max_range: 10,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::Circle2,
        effects: vec![
            EffectParams {
                effect: EffectType::Push,
                value1: 0.0,
                value2: 0.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::Damage,
                value1: 10.0,
                value2: 0.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1005 "plague": covid-shaped —
/// effects[0] is the propagation line (radius 3), effects[1] the actual
/// poison (6 + 3×jet for 2 turns), both NOT_REPLACEABLE.
fn harness_plague() -> ChipSpec {
    ChipSpec {
        id: 1005,
        cost: 3,
        min_range: 1,
        max_range: 7,
        launch_type: 1,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::Propagation,
                value1: 3.0,
                value2: 0.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::NOT_REPLACEABLE,
            },
            EffectParams {
                effect: EffectType::Poison,
                value1: 6.0,
                value2: 3.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::NOT_REPLACEABLE,
            },
        ],
        cooldown: 2,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1006 "blink": teleport, no LoS —
/// exercises the occupied-target precheck (USE_INVALID_TARGET, no RNG) and
/// the log-silent reposition.
fn harness_blink() -> ChipSpec {
    ChipSpec {
        id: 1006,
        cost: 2,
        min_range: 1,
        max_range: 12,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Teleport,
            value1: 0.0,
            value2: 0.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 2,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1007 "hook": attract over
/// FIRST_IN_LINE — cast at an adjacent in-line cell; the first entity on the
/// ray gets pulled toward the cast cell.
fn harness_hook() -> ChipSpec {
    ChipSpec {
        id: 1007,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 1,
        needs_los: true,
        max_uses: -1,
        area: Area::FirstInLine,
        effects: vec![EffectParams {
            effect: EffectType::Attract,
            value1: 0.0,
            value2: 0.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1008 "laser": damage (8 + 2×jet)
/// over LASER_LINE — line launch, fails silently when not aligned.
fn harness_laser() -> ChipSpec {
    ChipSpec {
        id: 1008,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 1,
        needs_los: true,
        max_uses: -1,
        area: Area::LaserLine,
        effects: vec![EffectParams {
            effect: EffectType::Damage,
            value1: 8.0,
            value2: 2.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1009 "storm": self-cast damage
/// (4 + 2×jet) over the ENEMIES area — hits every enemy wherever it stands.
fn harness_storm() -> ChipSpec {
    ChipSpec {
        id: 1009,
        cost: 2,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::Enemies,
        effects: vec![EffectParams {
            effect: EffectType::Damage,
            value1: 4.0,
            value2: 2.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1010 "blessing": self-cast
/// non-stackable strength buff (4 + 2×jet for 2 turns) over the ALLIES area.
fn harness_blessing() -> ChipSpec {
    ChipSpec {
        id: 1010,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::Allies,
        effects: vec![EffectParams {
            effect: EffectType::BuffStrength,
            value1: 4.0,
            value2: 2.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1011 "cure": instant heal
/// (12 + 4×jet, turns 0), wisdom-scaled, capped to the missing life.
fn harness_cure() -> ChipSpec {
    ChipSpec {
        id: 1011,
        cost: 2,
        min_range: 0,
        max_range: 6,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Heal,
            value1: 12.0,
            value2: 4.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1012 "regen": heal-over-time
/// (4 + 2×jet for 3 turns), self-cast, cooldown 2 (recasts land while the
/// previous effect is live → remove-previous on a heal).
fn harness_regen() -> ChipSpec {
    ChipSpec {
        id: 1012,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Heal,
            value1: 4.0,
            value2: 2.0,
            turns: 3,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 2,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1013 "wall": absolute shield
/// (6 + 2×jet for 2 turns), resistance-scaled, self-cast, non-stackable.
fn harness_wall() -> ChipSpec {
    ChipSpec {
        id: 1013,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::AbsoluteShield,
            value1: 6.0,
            value2: 2.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1014 "mirror": damage return
/// (8 + 2×jet for 2 turns), agility-scaled, self-cast — attackers can kill
/// themselves mid-turn (ENTITY_DIED silent abort).
fn harness_mirror() -> ChipSpec {
    ChipSpec {
        id: 1014,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::DamageReturn,
            value1: 8.0,
            value2: 2.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1015 "ice": shackle MP
/// (2 + 1×jet for 2 turns), magic-scaled, carried as a negative MP stat.
fn harness_ice() -> ChipSpec {
    ChipSpec {
        id: 1015,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 1,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::ShackleMp,
            value1: 2.0,
            value2: 1.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1016 "mud": shackle TP
/// (1 + 1×jet for 2 turns) — the shackled enemy loses casts immediately.
fn harness_mud() -> ChipSpec {
    ChipSpec {
        id: 1016,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 1,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::ShackleTp,
            value1: 1.0,
            value2: 1.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1017 "armor": relative shield
/// (10 + 5×jet for 2 turns), resistance-scaled, self-cast.
fn harness_armor() -> ChipSpec {
    ChipSpec {
        id: 1017,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::RelativeShield,
            value1: 10.0,
            value2: 5.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1018 "swap": permutation — the
/// caster trades places with the target (log-silent reposition).
fn harness_swap() -> ChipSpec {
    ChipSpec {
        id: 1018,
        cost: 1,
        min_range: 1,
        max_range: 10,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Permutation,
            value1: 0.0,
            value2: 0.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1019 "spring": [repel, damage
/// 6 + 2×jet] — the repel line is DEAD in this generator (empty class).
fn harness_spring() -> ChipSpec {
    ChipSpec {
        id: 1019,
        cost: 2,
        min_range: 1,
        max_range: 7,
        launch_type: 1,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::Repel,
                value1: 0.0,
                value2: 0.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::Damage,
                value1: 6.0,
                value2: 2.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1020 "fortress": vitality
/// (30 + 10×jet for 2 turns), self-cast — permanent max-life bump + heal.
fn harness_fortress() -> ChipSpec {
    ChipSpec {
        id: 1020,
        cost: 2,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Vitality,
            value1: 30.0,
            value2: 10.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1021 "reflex": agility buff
/// (25 + 10×jet for 2 turns), science-scaled, self-cast.
fn harness_reflex() -> ChipSpec {
    ChipSpec {
        id: 1021,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::BuffAgility,
            value1: 25.0,
            value2: 10.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1022 "haste": MP buff
/// (2 + 1×jet for 2 turns), science-scaled, self-cast.
fn harness_haste() -> ChipSpec {
    ChipSpec {
        id: 1022,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::BuffMp,
            value1: 2.0,
            value2: 1.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1023 "focus": TP buff
/// (1 + 1×jet for 2 turns), science-scaled, self-cast — usable immediately.
fn harness_focus() -> ChipSpec {
    ChipSpec {
        id: 1023,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::BuffTp,
            value1: 1.0,
            value2: 1.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1024 "sage" — wisdom buff 20+5×jet for 2 turns, self-cast
/// (science-scaled; wisdom feeds life-steal and heals).
fn harness_sage() -> ChipSpec {
    ChipSpec {
        id: 1024,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::BuffWisdom,
            value1: 20.0,
            value2: 5.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1025 "brick" — resistance buff 15+5×jet for 2 turns,
/// self-cast (science-scaled).
fn harness_brick() -> ChipSpec {
    ChipSpec {
        id: 1025,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::BuffResistance,
            value1: 15.0,
            value2: 5.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1026 "weaken" — strength shackle 10+3×jet for 2 turns,
/// range 1–8 (magic-scaled, carried as a negative STR stat).
fn harness_weaken() -> ChipSpec {
    ChipSpec {
        id: 1026,
        cost: 1,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::ShackleStrength,
            value1: 10.0,
            value2: 3.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1027 "numb" — agility shackle 10+3×jet for 2 turns,
/// range 1–8 (cuts the target's crit chance).
fn harness_numb() -> ChipSpec {
    ChipSpec {
        id: 1027,
        cost: 1,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::ShackleAgility,
            value1: 10.0,
            value2: 3.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1028 "dull" — wisdom shackle 8+2×jet for 2 turns,
/// range 1–8 (cuts the target's life-steal and heals).
fn harness_dull() -> ChipSpec {
    ChipSpec {
        id: 1028,
        cost: 1,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::ShackleWisdom,
            value1: 8.0,
            value2: 2.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1029 "hush" — magic shackle 5+2×jet for 2 turns, range 1–8
/// (value-only pin — our leeks have 0 magic).
fn harness_hush() -> ChipSpec {
    ChipSpec {
        id: 1029,
        cost: 1,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::ShackleMagic,
            value1: 5.0,
            value2: 2.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1030 "cleanse" — `[antidote, removeShackles]` instant,
/// self-cast: each removed effect logs `[303]`, then `[307]`/`[308]`.
fn harness_cleanse() -> ChipSpec {
    ChipSpec {
        id: 1030,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::Antidote,
                value1: 0.0,
                value2: 0.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::RemoveShackles,
                value1: 0.0,
                value2: 0.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1031 "unravel" — debuff 40+10×jet percent, instant,
/// range 1–8 (truncating cast, reduces every non-IRREDUCTIBLE effect).
fn harness_unravel() -> ChipSpec {
    ChipSpec {
        id: 1031,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Debuff,
            value1: 40.0,
            value2: 10.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1032 "javelin" — damage 5+2×jet, **launch type 9**, range
/// 2–7, LoS: range-gates like LAUNCH_TYPE_LINE, but cast-cell search goes
/// through the generateMask path (not the line-walking branch).
fn harness_javelin() -> ChipSpec {
    ChipSpec {
        id: 1032,
        cost: 2,
        min_range: 2,
        max_range: 7,
        launch_type: 9,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Damage,
            value1: 5.0,
            value2: 2.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1033 "comet" — damage 5+2×jet, **launch type 10**, range
/// 1–8, no LoS: diagonal-only casts with the len=max mask special case.
fn harness_comet() -> ChipSpec {
    ChipSpec {
        id: 1033,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 10,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Damage,
            value1: 5.0,
            value2: 2.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1034 "statue" — ADD_STATE STATIC for 2 turns, range 1–8:
/// the target can't move / be slid / be the *target* of a permutation.
fn harness_statue() -> ChipSpec {
    ChipSpec {
        id: 1034,
        cost: 1,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::AddState,
            value1: 11.0, // EntityState.STATIC
            value2: 0.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1035 "ghost" — ADD_STATE INVINCIBLE for 2 turns, self-cast:
/// zeroes incoming damage after shields (still logs 0), silences poison
/// ticks, and blocks return damage when the invincible entity attacks.
fn harness_ghost() -> ChipSpec {
    ChipSpec {
        id: 1035,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::AddState,
            value1: 3.0, // EntityState.INVINCIBLE
            value2: 0.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1036 "curse" — ADD_STATE UNHEALABLE for 2 turns, range 1–8:
/// heals skip silently (before the log) and life-steal is blocked.
fn harness_curse() -> ChipSpec {
    ChipSpec {
        id: 1036,
        cost: 1,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::AddState,
            value1: 2.0, // EntityState.UNHEALABLE
            value2: 0.0,
            turns: 2,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1037 "toxin" — aftereffect 8+3×jet for 2 turns (science-
/// scaled, applies at cast AND ticks each turn), paired with an
/// `AllyKilledToAgility` line — a DEAD effect (empty class upstream).
fn harness_toxin() -> ChipSpec {
    ChipSpec {
        id: 1037,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::Aftereffect,
                value1: 8.0,
                value2: 3.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::AllyKilledToAgility,
                value1: 100.0,
                value2: 0.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1038 "reaper" — [damage 8+2×jet, STEAL_LIFE on-caster]: the
/// steal line heals the caster by the damage line's total value
/// (`previousEffectTotalValue`).
fn harness_reaper() -> ChipSpec {
    ChipSpec {
        id: 1038,
        cost: 2,
        min_range: 1,
        max_range: 7,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::Damage,
                value1: 8.0,
                value2: 2.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::StealLife,
                value1: 0.0,
                value2: 0.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::ON_CASTER,
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1039 "leech" — [damage 6+2×jet, STEAL_ABSOLUTE_SHIELD
/// on-caster for 2 turns]: carries the damage line's total value as an
/// absolute shield on the caster.
fn harness_leech() -> ChipSpec {
    ChipSpec {
        id: 1039,
        cost: 2,
        min_range: 1,
        max_range: 7,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::Damage,
                value1: 6.0,
                value2: 2.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::StealAbsoluteShield,
                value1: 0.0,
                value2: 0.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::ON_CASTER,
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1040 "cataclysm" — [NOVA_DAMAGE 10+4×jet, LIFE_DAMAGE
/// 4+2×jet]: pure erosion + caster-life-scaled damage.
fn harness_cataclysm() -> ChipSpec {
    ChipSpec {
        id: 1040,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::NovaDamage,
                value1: 10.0,
                value2: 4.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::LifeDamage,
                value1: 4.0,
                value2: 2.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1041 "doom" — KILL: value = the target's full life;
/// `ActionKill` logs the TARGET fid twice (upstream ctor bug); the
/// invincible check is commented out upstream.
fn harness_doom() -> ChipSpec {
    ChipSpec {
        id: 1041,
        cost: 4,
        min_range: 1,
        max_range: 6,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Kill,
            value1: 0.0,
            value2: 0.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1042 "mutation" — raw STR/AGI (IRREDUCTIBLE) + POWER/MAGIC
/// buffs for 2 turns, self-cast.
fn harness_mutation() -> ChipSpec {
    ChipSpec {
        id: 1042,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::RawBuffStrength,
                value1: 10.0,
                value2: 5.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::IRREDUCTIBLE,
            },
            EffectParams {
                effect: EffectType::RawBuffAgility,
                value1: 10.0,
                value2: 5.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::IRREDUCTIBLE,
            },
            EffectParams {
                effect: EffectType::RawBuffPower,
                value1: 5.0,
                value2: 2.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::RawBuffMagic,
                value1: 10.0,
                value2: 5.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1043 "clarity" — raw SCIENCE/WISDOM/RESISTANCE buffs for 2
/// turns, self-cast (science feeds later science-scaled casts).
fn harness_clarity() -> ChipSpec {
    ChipSpec {
        id: 1043,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::RawBuffScience,
                value1: 20.0,
                value2: 10.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::RawBuffWisdom,
                value1: 10.0,
                value2: 5.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::RawBuffResistance,
                value1: 10.0,
                value2: 5.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1044 "bulwark" — raw shields + RAW_BUFF_MP/TP for 2 turns,
/// self-cast (the MP/TP lines use ×targetCount, NO aoe factor).
fn harness_bulwark() -> ChipSpec {
    ChipSpec {
        id: 1044,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::RawAbsoluteShield,
                value1: 8.0,
                value2: 4.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::RawRelativeShield,
                value1: 8.0,
                value2: 4.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::RawBuffMp,
                value1: 1.0,
                value2: 1.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::RawBuffTp,
                value1: 1.0,
                value2: 1.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1045 "rupture" — [VULNERABILITY 15+5×jet,
/// ABSOLUTE_VULNERABILITY 10+5×jet] for 2 turns: negative shield carriers.
fn harness_rupture() -> ChipSpec {
    ChipSpec {
        id: 1045,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::Vulnerability,
                value1: 15.0,
                value2: 5.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::AbsoluteVulnerability,
                value1: 10.0,
                value2: 5.0,
                turns: 2,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1046 "purge" — TOTAL_DEBUFF 30+20×jet percent: like debuff
/// (truncating cast) but reduces IRREDUCTIBLE effects too.
fn harness_purge() -> ChipSpec {
    ChipSpec {
        id: 1046,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::TotalDebuff,
            value1: 30.0,
            value2: 20.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// Harness chip 1047 "transfusion" — [NOVA_VITALITY 15+5×jet, RAW_HEAL
/// 10+5×jet], self-cast: max-life bump WITHOUT heal, then a raw heal into
/// the new headroom.
fn harness_transfusion() -> ChipSpec {
    ChipSpec {
        id: 1047,
        cost: 1,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![
            EffectParams {
                effect: EffectType::NovaVitality,
                value1: 15.0,
                value2: 5.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
            EffectParams {
                effect: EffectType::RawHeal,
                value1: 10.0,
                value2: 5.0,
                turns: 0,
                targets: EffectTargets::all(),
                modifiers: EffectModifiers::empty(),
            },
        ],
        cooldown: 0,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1048 "spawn": TYPE_SUMMON →
/// bulb template 1001, range 1–8 free launch with LoS, TEAM cooldown 2 —
/// exercises the summonEntity ladder (UseChip + Invocation logs, exactly one
/// crit getDouble on success, failures draw NO RNG, no addItemUse) and the
/// useChip intercept (BULB_WITHOUT_AI idle bulbs).
fn harness_spawn() -> ChipSpec {
    ChipSpec {
        id: 1048,
        cost: 2,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: true,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Summon,
            value1: 1001.0,
            value2: 0.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 2,
        team_cooldown: true,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 84 "revive": TYPE_RESURRECT,
/// range 1–8 free launch without LoS, cooldown 4. The id is NOT synthetic:
/// `ChipClass.resurrect` hardwires `FightConstants.CHIP_RESURRECTION` (84),
/// so the corpus registers a castable template under the real id (the real
/// chip costs 15 TP — more than the harness leek's 6). Exercises the
/// resurrectEntity ladder (canUseAttack -4 BEFORE hasCooldown -3, dead-only
/// targets -6, one crit getDouble on success, ActionResurrect + half-life
/// revival, Order re-insertion before the next initial-order survivor).
fn harness_revive() -> ChipSpec {
    ChipSpec {
        id: 84,
        cost: 3,
        min_range: 1,
        max_range: 8,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::Resurrect,
            value1: 0.0,
            value2: 0.0,
            turns: 0,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 4,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — synthetic chip 1049 "colossus":
/// TYPE_MULTIPLY_STATS ×2 for 3 turns, self-cast, cooldown 2 — exercises
/// `EffectMultiplyStats` (base-stat ×(factor−1) buffs, the first-apply vs
/// replacement max-life delta — cooldown 2 < turns 3 lets a recast replace
/// the live effect — and the ratio-preserving silent heal).
fn harness_colossus() -> ChipSpec {
    ChipSpec {
        id: 1049,
        cost: 2,
        min_range: 0,
        max_range: 0,
        launch_type: 7,
        needs_los: false,
        max_uses: -1,
        area: Area::SingleCell,
        effects: vec![EffectParams {
            effect: EffectType::MultiplyStats,
            value1: 2.0,
            value2: 0.0,
            turns: 3,
            targets: EffectTargets::all(),
            modifiers: EffectModifiers::empty(),
        }],
        cooldown: 2,
        team_cooldown: false,
        initial_cooldown: 0,
        level: 1,
    }
}

/// `Harness.registerChips` — bulb template 1001 "harness_bulb": stat ranges
/// scaled by the OWNER's level (10 → coeff 1/30, bulb_base truncating),
/// ×1.2 on a critical summon. Chips laser (damage) + cure (heal).
fn harness_bulb() -> BulbTemplate {
    BulbTemplate {
        id: 1001,
        name: "harness_bulb".to_string(),
        life: (100, 400),
        strength: (50, 200),
        wisdom: (0, 100),
        agility: (30, 300),
        resistance: (0, 0),
        science: (0, 100),
        magic: (0, 0),
        tp: (4, 8),
        mp: (3, 6),
        chips: vec![1008, 1011],
    }
}

fn run() -> Result<serde_json::Value> {
    let args: Vec<String> = std::env::args().collect();
    let (ai1, ai2) = match args.as_slice() {
        [_, a, b] | [_, a, b, _] => (a.clone(), b.clone()),
        _ => {
            return Err(anyhow!(
                "usage: official-fight <ai1.leek> <ai2.leek> [seed]"
            ));
        }
    };
    let seed: i64 = match args.get(3) {
        Some(s) => s.parse().with_context(|| format!("invalid seed {s:?}"))?,
        None => 1,
    };

    // The fight functions must resolve at compile time, same as `miku fight`,
    // and the fight constants must fold to literals like the harness's
    // `leekc --fold-constants` invocation that generated the goldens.
    leek_recipes::load_and_register_libraries(["leekwars"])
        .map_err(|e| anyhow!("registering the leekwars library: {e}"))?;
    leek_recipes::activate_leekwars_constant_folding();

    let mut state = State::new(seed);
    state.add_entity(0, harness_leek(1, "AI_1"));
    state.add_entity(1, harness_leek(2, "AI_2"));
    state.weapon_specs.insert(37, harness_pistol());
    state.chip_specs.insert(1001, harness_venom());
    state.chip_specs.insert(1002, harness_protein());
    state.chip_specs.insert(1003, harness_magnet());
    state.chip_specs.insert(1004, harness_glove());
    state.chip_specs.insert(1005, harness_plague());
    state.chip_specs.insert(1006, harness_blink());
    state.chip_specs.insert(1007, harness_hook());
    state.chip_specs.insert(1008, harness_laser());
    state.chip_specs.insert(1009, harness_storm());
    state.chip_specs.insert(1010, harness_blessing());
    state.chip_specs.insert(1011, harness_cure());
    state.chip_specs.insert(1012, harness_regen());
    state.chip_specs.insert(1013, harness_wall());
    state.chip_specs.insert(1014, harness_mirror());
    state.chip_specs.insert(1015, harness_ice());
    state.chip_specs.insert(1016, harness_mud());
    state.chip_specs.insert(1017, harness_armor());
    state.chip_specs.insert(1018, harness_swap());
    state.chip_specs.insert(1019, harness_spring());
    state.chip_specs.insert(1020, harness_fortress());
    state.chip_specs.insert(1021, harness_reflex());
    state.chip_specs.insert(1022, harness_haste());
    state.chip_specs.insert(1023, harness_focus());
    state.chip_specs.insert(1024, harness_sage());
    state.chip_specs.insert(1025, harness_brick());
    state.chip_specs.insert(1026, harness_weaken());
    state.chip_specs.insert(1027, harness_numb());
    state.chip_specs.insert(1028, harness_dull());
    state.chip_specs.insert(1029, harness_hush());
    state.chip_specs.insert(1030, harness_cleanse());
    state.chip_specs.insert(1031, harness_unravel());
    state.chip_specs.insert(1032, harness_javelin());
    state.chip_specs.insert(1033, harness_comet());
    state.chip_specs.insert(1034, harness_statue());
    state.chip_specs.insert(1035, harness_ghost());
    state.chip_specs.insert(1036, harness_curse());
    state.chip_specs.insert(1037, harness_toxin());
    state.chip_specs.insert(1038, harness_reaper());
    state.chip_specs.insert(1039, harness_leech());
    state.chip_specs.insert(1040, harness_cataclysm());
    state.chip_specs.insert(1041, harness_doom());
    state.chip_specs.insert(1042, harness_mutation());
    state.chip_specs.insert(1043, harness_clarity());
    state.chip_specs.insert(1044, harness_bulwark());
    state.chip_specs.insert(1045, harness_rupture());
    state.chip_specs.insert(1046, harness_purge());
    state.chip_specs.insert(1047, harness_transfusion());
    state.chip_specs.insert(1048, harness_spawn());
    state.chip_specs.insert(84, harness_revive());
    state.chip_specs.insert(1049, harness_colossus());
    state.bulb_templates.insert(1001, harness_bulb());

    let mut ais = HashMap::new();
    for (fid, path) in [(0_usize, &ai1), (1, &ai2)] {
        ais.insert(fid, leek_scenario::compile_ai(Path::new(path), 4, false)?);
    }

    let opts = leek_generator::NativeOptions::release()
        .with_lang(4, false)
        .with_link_game(true);
    run_official_fight(state, &ais, &[0], &opts).map_err(|e| anyhow!("running fight: {e}"))
}

fn main() -> ExitCode {
    match run() {
        Ok(outcome) => {
            println!("{outcome}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("official-fight: {e:#}");
            ExitCode::FAILURE
        }
    }
}
