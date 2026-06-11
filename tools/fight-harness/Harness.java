import java.util.HashMap;

import com.leekwars.generator.Generator;
import com.leekwars.generator.fight.Fight;
import com.leekwars.generator.leek.FarmerLog;
import com.leekwars.generator.leek.Leek;
import com.leekwars.generator.leek.LeekLog;
import com.leekwars.generator.leek.RegisterManager;
import com.leekwars.generator.outcome.Outcome;
import com.leekwars.generator.test.LocalTrophyManager;

import com.leekwars.generator.bulbs.BulbTemplate;
import com.leekwars.generator.bulbs.Bulbs;
import com.leekwars.generator.chips.Chip;
import com.leekwars.generator.chips.ChipType;
import com.leekwars.generator.chips.Chips;
import com.leekwars.generator.util.Json;
import com.leekwars.generator.weapons.Weapon;
import com.leekwars.generator.weapons.Weapons;

import leekscript.compiler.AIFile;
import leekscript.compiler.LeekScript;
import leekscript.compiler.Options;
import leekscript.runner.AI;

/**
 * Standalone fight runner that plugs AI classes emitted by the Rust
 * Leekscript compiler into the (unmodified) leek-wars-generator.
 *
 * The generator always rebuilds each entity's AI in {@code Fight.startFight}
 * via {@code EntityAI.build(generator, entity.getAIFile(), entity)}, which
 * calls {@code file.compile(options)} on the entity's {@link AIFile}. We
 * exploit that single public, non-final hook: {@link InjectedAIFile}
 * overrides {@code compile} to hand back our already-compiled
 * {@code EntityAI} subclass instead of compiling Leekscript source. No
 * generator code is touched, so it can be updated freely.
 *
 * Usage: {@code java Harness <AIClass1> <AIClass2> [seed]}
 * The two AI classes must be on the classpath (we javac them alongside
 * this harness against the generator's runtime classpath). Prints the
 * fight {@link Outcome} as JSON on the line after the {@code @@OUTCOME@@}
 * marker, keeping it separable from the generator's own logging.
 */
public class Harness {

    /** Marker the wrapper greps for to extract the JSON from stdout. */
    static final String MARKER = "@@OUTCOME@@";

    /**
     * An {@link AIFile} whose {@code compile} returns a pre-built AI
     * instance rather than compiling Leekscript source.
     */
    static final class InjectedAIFile extends AIFile {
        private final Class<?> aiClass;

        InjectedAIFile(String path, int owner, Class<?> aiClass) {
            super(path, "", System.currentTimeMillis(), LeekScript.LATEST_VERSION, owner, false);
            this.aiClass = aiClass;
        }

        // Narrows the parent's checked throws to none; instantiation
        // failures surface as an unchecked exception.
        @Override
        public AI compile(Options options) {
            try {
                return (AI) aiClass.getDeclaredConstructor().newInstance();
            } catch (ReflectiveOperationException e) {
                throw new RuntimeException("could not instantiate emitted AI " + aiClass.getName(), e);
            }
        }
    }

    /** Stock leek stats, mirroring the generator's FightTestBase.defaultLeek.
     *  (ram bumped 30 → 50: `Entity.addChip` silently drops chips beyond
     *  getRAM(), and the corpus now equips more than 30 — ram isn't part of
     *  the outcome JSON, so goldens are unaffected.) */
    static Leek defaultLeek(int id, String name) {
        return new Leek(id, name, 0, 10, 500, 6, 7, 100, 100, 10, 50, 10, 0, 0, 8, 50, 0, false, 0, 0, "", 0, "", "", "", 0);
    }

    /** Pistol = WEAPON_PISTOL (item id 37): range 1–7, ~15 damage, 3 TP. */
    static final int PISTOL_WEAPON_ID = 37;

    /** Register a damaging pistol. The generator's bundled
     *  `data/weapons.json` fails to load (missing `max_uses`), so its
     *  registry is empty; we register an equivalent synthetic pistol —
     *  keyed by id 37 so `addWeapon(getWeapon(37))` + `setWeapon(37)` work
     *  — carrying a damage effect (type 1, value1=15) so fights deal real
     *  damage and reach a KO. Done in our harness, not the generator. */
    static void registerPistol() {
        if (Weapons.getWeapon(PISTOL_WEAPON_ID) != null) {
            return;
        }
        var effects = Json.createArray();
        var dmg = Json.createObject();
        dmg.put("id", 1);
        dmg.put("type", 1); // EFFECT_DAMAGE
        dmg.put("value1", 15);
        dmg.put("value2", 5);
        dmg.put("targets", 31);
        dmg.put("turns", 0);
        dmg.put("modifiers", 0);
        effects.add(dmg);
        Weapons.addWeapon(new Weapon(
            PISTOL_WEAPON_ID, /*cost*/ 3, /*minRange*/ 1, /*maxRange*/ 7, effects,
            /*launchType*/ (byte) 1, /*area*/ (byte) 1, /*los*/ true,
            /*template*/ PISTOL_WEAPON_ID, "pistol", Json.createArray(),
            /*maxUses*/ -1, /*forgotten*/ false));
    }

    /** Venom-like poison chip (timed effect + entity cooldown + initial cooldown). */
    static final int VENOM_CHIP_ID = 1001;
    /** Protein-like strength buff chip (stackable, no cooldown, self-cast). */
    static final int PROTEIN_CHIP_ID = 1002;
    /** Grapple-like attract chip (self-cast CIRCLE_3 — pulls everything in). */
    static final int MAGNET_CHIP_ID = 1003;
    /** Boxing-glove-like push + splash-damage chip (CIRCLE_2, area factors). */
    static final int GLOVE_CHIP_ID = 1004;
    /** Covid-like propagating poison chip (propagation + NOT_REPLACEABLE). */
    static final int PLAGUE_CHIP_ID = 1005;
    /** Teleportation-like chip (occupied-cell precheck, silent reposition). */
    static final int BLINK_CHIP_ID = 1006;
    /** Grapple-like FIRST_IN_LINE attract chip (cast at an in-line cell, the
     *  first entity on the ray gets pulled toward the cast cell). */
    static final int HOOK_CHIP_ID = 1007;
    /** Laser-like LASER_LINE damage chip (hits every cell along the ray). */
    static final int LASER_CHIP_ID = 1008;
    /** Enemies-area damage chip (hits every enemy wherever it stands). */
    static final int STORM_CHIP_ID = 1009;
    /** Allies-area non-stackable strength buff chip. */
    static final int BLESSING_CHIP_ID = 1010;
    /** Instant-heal chip (wisdom-scaled, capped to missing life). */
    static final int CURE_CHIP_ID = 1011;
    /** Heal-over-time chip (self-cast, ticks at the target's turn starts). */
    static final int REGEN_CHIP_ID = 1012;
    /** Absolute-shield chip (resistance-scaled, self-cast, replace-on-recast). */
    static final int WALL_CHIP_ID = 1013;
    /** Damage-return chip (agility-scaled, self-cast). */
    static final int MIRROR_CHIP_ID = 1014;
    /** Shackle-MP chip (magic-scaled, carried as a negative MP stat). */
    static final int ICE_CHIP_ID = 1015;
    /** Shackle-TP chip (magic-scaled, carried as a negative TP stat). */
    static final int MUD_CHIP_ID = 1016;
    /** Relative-shield chip (resistance-scaled, self-cast). */
    static final int ARMOR_CHIP_ID = 1017;
    /** Permutation chip (swap caster and target — log-silent reposition). */
    static final int SWAP_CHIP_ID = 1018;
    /** Repel + damage chip (repel is a DEAD effect in this generator). */
    static final int SPRING_CHIP_ID = 1019;
    /** Vitality chip (wisdom-scaled permanent max-life increase + heal). */
    static final int FORTRESS_CHIP_ID = 1020;
    /** Agility buff chip (science-scaled — feeds crit rolls + return). */
    static final int REFLEX_CHIP_ID = 1021;
    /** MP buff chip (science-scaled — extra movement). */
    static final int HASTE_CHIP_ID = 1022;
    /** TP buff chip (science-scaled — the extra TP lands immediately). */
    static final int FOCUS_CHIP_ID = 1023;
    /** Wisdom buff chip (science-scaled — feeds life-steal and heals). */
    static final int SAGE_CHIP_ID = 1024;
    /** Resistance buff chip (science-scaled — feeds later shield casts). */
    static final int BRICK_CHIP_ID = 1025;
    /** Shackle-strength chip (magic-scaled, negative STR stat). */
    static final int WEAKEN_CHIP_ID = 1026;
    /** Shackle-agility chip (magic-scaled — cuts the target's crit rolls). */
    static final int NUMB_CHIP_ID = 1027;
    /** Shackle-wisdom chip (magic-scaled — cuts the target's life-steal). */
    static final int DULL_CHIP_ID = 1028;
    /** Shackle-magic chip (magic-scaled, negative MAGIC stat). */
    static final int HUSH_CHIP_ID = 1029;
    /** Antidote + remove-shackles chip (clears poisons and shackles). */
    static final int CLEANSE_CHIP_ID = 1030;
    /** Debuff chip (percent reduction of the target's effects). */
    static final int UNRAVEL_CHIP_ID = 1031;
    /** Launch-type-9 damage chip (line cast positions via the MASK path,
     *  not the LAUNCH_TYPE_LINE walking branch of cast-cell search). */
    static final int JAVELIN_CHIP_ID = 1032;
    /** Launch-type-10 damage chip (diagonal-only casts, len=max mask). */
    static final int COMET_CHIP_ID = 1033;
    /** Add-state STATIC chip (target can't move / be slid / be swapped). */
    static final int STATUE_CHIP_ID = 1034;
    /** Add-state INVINCIBLE chip (zeroes incoming damage and poison ticks,
     *  blocks incoming return damage when the invincible entity attacks). */
    static final int GHOST_CHIP_ID = 1035;
    /** Add-state UNHEALABLE chip (silently skips heals and life-steal). */
    static final int CURSE_CHIP_ID = 1036;
    /** Aftereffect chip (science-scaled damage that also ticks per turn) —
     *  paired with an AllyKilledToAgility line (a DEAD effect, empty class). */
    static final int TOXIN_CHIP_ID = 1037;
    /** [damage, STEAL_LIFE on-caster] chip — the steal line heals the caster
     *  by the damage line's total value (previousEffectTotalValue). */
    static final int REAPER_CHIP_ID = 1038;
    /** [damage, STEAL_ABSOLUTE_SHIELD on-caster] chip — carries the damage
     *  line's total value as an absolute shield on the caster. */
    static final int LEECH_CHIP_ID = 1039;
    /** [NOVA_DAMAGE, LIFE_DAMAGE] chip — pure erosion + caster-life-scaled
     *  damage. */
    static final int CATACLYSM_CHIP_ID = 1040;
    /** KILL chip — sets the target's life to 0 (the invincible check is
     *  commented out upstream; ActionKill logs the target fid twice). */
    static final int DOOM_CHIP_ID = 1041;
    /** Raw stat buffs (STR/AGI IRREDUCTIBLE + POWER/MAGIC), self-cast. */
    static final int MUTATION_CHIP_ID = 1042;
    /** Raw stat buffs (SCIENCE/WISDOM/RESISTANCE), self-cast. */
    static final int CLARITY_CHIP_ID = 1043;
    /** Raw shields + RAW_BUFF_MP/TP (targetCount-shaped, no aoe), self-cast. */
    static final int BULWARK_CHIP_ID = 1044;
    /** [VULNERABILITY, ABSOLUTE_VULNERABILITY] chip — negative shields. */
    static final int RUPTURE_CHIP_ID = 1045;
    /** TOTAL_DEBUFF chip — like debuff but reduces IRREDUCTIBLE effects too. */
    static final int PURGE_CHIP_ID = 1046;
    /** [NOVA_VITALITY, RAW_HEAL] chip — max-life bump (no heal) + raw heal
     *  into the new headroom. */
    static final int TRANSFUSION_CHIP_ID = 1047;
    /** TYPE_SUMMON chip → bulb template 1001 — the summon ladder (one crit
     *  getDouble on success, failed checks draw NO RNG), Order insertion
     *  right after the owner, and the TEAM cooldown path. */
    static final int SPAWN_CHIP_ID = 1048;
    /** TYPE_RESURRECT chip — the id is NOT synthetic: `ChipClass.resurrect`
     *  hardwires `FightConstants.CHIP_RESURRECTION` (84), so the corpus
     *  registers a castable template under the real id (the real chip costs
     *  15 TP — more than the harness leek's 6). */
    static final int REVIVE_CHIP_ID = 84;
    /** TYPE_MULTIPLY_STATS ×2 for 3 turns, self-cast — EffectMultiplyStats
     *  (first-apply vs replacement max-life delta, silent ratio heal). */
    static final int COLOSSUS_CHIP_ID = 1049;
    /** Synthetic bulb template id — its own namespace, distinct from chips
     *  (real templates occupy 1–12 in data/summons.json). */
    static final int HARNESS_BULB_ID = 1001;

    /** Register the synthetic chips of the conformance corpus. Like the
     *  pistol, the generator's bundled chip data isn't loaded, so we register
     *  the exact templates the Rust twin (`official-fight`) hardcodes:
     *  - 1001 "venom": poison 10+5×jet for 2 turns, 2 TP, range 1–7 (LoS),
     *    cooldown 2, initial cooldown 1 — exercises AddEffect/RemoveEffect,
     *    poison start-turn ticks, remove-previous on recast, and both the
     *    entity and the initial cooldown paths;
     *  - 1002 "protein": stackable strength buff 5+5×jet for 3 turns, 0 TP,
     *    self-only (range 0) — exercises stack-merge (ActionStackEffect),
     *    buff stats feeding weapon damage, and the `cost > 0` TP-check quirk. */
    static void registerChips() {
        if (Chips.getChip(VENOM_CHIP_ID) != null) {
            return;
        }
        var venomEffects = Json.createArray();
        var poison = Json.createObject();
        poison.put("id", 13); // EFFECT_POISON
        poison.put("value1", 10);
        poison.put("value2", 5);
        poison.put("turns", 2);
        poison.put("targets", 31);
        poison.put("modifiers", 0);
        venomEffects.add(poison);
        Chips.addChip(new Chip(
            VENOM_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 7, venomEffects,
            /*launchType*/ (byte) 1, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 2, /*teamCooldown*/ false, /*initialCooldown*/ 1,
            /*level*/ 1, /*template*/ VENOM_CHIP_ID, "venom", ChipType.POISON,
            /*maxUses*/ -1));

        var proteinEffects = Json.createArray();
        var buff = Json.createObject();
        buff.put("id", 3); // EFFECT_BUFF_STRENGTH
        buff.put("value1", 5);
        buff.put("value2", 5);
        buff.put("turns", 3);
        buff.put("targets", 14); // ALLIES | CASTER | NON_SUMMONS
        buff.put("modifiers", 1); // STACKABLE
        proteinEffects.add(buff);
        Chips.addChip(new Chip(
            PROTEIN_CHIP_ID, /*cost*/ 0, /*minRange*/ 0, /*maxRange*/ 0, proteinEffects,
            /*launchType*/ (byte) 1, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ PROTEIN_CHIP_ID, "protein", ChipType.BOOST,
            /*maxUses*/ -1));

        // 1003 "magnet": self-cast (range 0) ATTRACT over CIRCLE_3 — pulls
        // every entity within 3 cells straight to the caster (the attract
        // walk targets the cast cell and stops on the first occupied cell).
        var magnetEffects = Json.createArray();
        var attract = Json.createObject();
        attract.put("id", 46); // EFFECT_ATTRACT
        attract.put("value1", 0);
        attract.put("value2", 0);
        attract.put("turns", 0);
        attract.put("targets", 31);
        attract.put("modifiers", 0);
        magnetEffects.add(attract);
        Chips.addChip(new Chip(
            MAGNET_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, magnetEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 5, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ MAGNET_CHIP_ID, "magnet", ChipType.TACTIC,
            /*maxUses*/ -1));

        // 1004 "glove": PUSH + splash damage over CIRCLE_2, no LoS — cast at
        // the cell *behind* the enemy so the push direction (away from the
        // caster, toward the cast cell) lines up, and the damage lands with
        // an area factor of 0.8 (distance 1 from the cast cell).
        var gloveEffects = Json.createArray();
        var push = Json.createObject();
        push.put("id", 51); // EFFECT_PUSH
        push.put("value1", 0);
        push.put("value2", 0);
        push.put("turns", 0);
        push.put("targets", 31);
        push.put("modifiers", 0);
        gloveEffects.add(push);
        var splash = Json.createObject();
        splash.put("id", 1); // EFFECT_DAMAGE
        splash.put("value1", 10);
        splash.put("value2", 0);
        splash.put("turns", 0);
        splash.put("targets", 31);
        splash.put("modifiers", 0);
        gloveEffects.add(splash);
        Chips.addChip(new Chip(
            GLOVE_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 10, gloveEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 4, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ GLOVE_CHIP_ID, "glove", ChipType.DAMAGE,
            /*maxUses*/ -1));

        // 1005 "plague": covid-shaped — effects[0] is the PROPAGATION line
        // (radius 3), effects[1] the actual poison (6+3×jet for 2 turns),
        // both NOT_REPLACEABLE. The poison re-propagates from its victims
        // at the end of their turns (back to the original caster too).
        var plagueEffects = Json.createArray();
        var prop = Json.createObject();
        prop.put("id", 43); // EFFECT_PROPAGATION
        prop.put("value1", 3);
        prop.put("value2", 0);
        prop.put("turns", 0);
        prop.put("targets", 31);
        prop.put("modifiers", 8); // NOT_REPLACEABLE
        plagueEffects.add(prop);
        var plaguePoison = Json.createObject();
        plaguePoison.put("id", 13); // EFFECT_POISON
        plaguePoison.put("value1", 6);
        plaguePoison.put("value2", 3);
        plaguePoison.put("turns", 2);
        plaguePoison.put("targets", 31);
        plaguePoison.put("modifiers", 8); // NOT_REPLACEABLE
        plagueEffects.add(plaguePoison);
        Chips.addChip(new Chip(
            PLAGUE_CHIP_ID, /*cost*/ 3, /*minRange*/ 1, /*maxRange*/ 7, plagueEffects,
            /*launchType*/ (byte) 1, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 2, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ PLAGUE_CHIP_ID, "plague", ChipType.POISON,
            /*maxUses*/ -1));

        // 1006 "blink": TELEPORT, no LoS — exercises the occupied-target
        // precheck (USE_INVALID_TARGET, no RNG drawn) and the log-silent
        // reposition.
        var blinkEffects = Json.createArray();
        var teleport = Json.createObject();
        teleport.put("id", 10); // EFFECT_TELEPORT
        teleport.put("value1", 0);
        teleport.put("value2", 0);
        teleport.put("turns", 0);
        teleport.put("targets", 31);
        teleport.put("modifiers", 0);
        blinkEffects.add(teleport);
        Chips.addChip(new Chip(
            BLINK_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 12, blinkEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 2, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ BLINK_CHIP_ID, "blink", ChipType.TACTIC,
            /*maxUses*/ -1));

        // 1007 "hook": ATTRACT over FIRST_IN_LINE — cast at an adjacent
        // in-line cell; the area resolves the first entity on the ray (the
        // walk extends past the cast cell up to maxRange) and the attract
        // pulls it toward the cast cell. Also pins the aiming-at-the-first-
        // entity LoS rule in canUseAttack.
        var hookEffects = Json.createArray();
        var hookAttract = Json.createObject();
        hookAttract.put("id", 46); // EFFECT_ATTRACT
        hookAttract.put("value1", 0);
        hookAttract.put("value2", 0);
        hookAttract.put("turns", 0);
        hookAttract.put("targets", 31);
        hookAttract.put("modifiers", 0);
        hookEffects.add(hookAttract);
        Chips.addChip(new Chip(
            HOOK_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, hookEffects,
            /*launchType*/ (byte) 1, /*area*/ (byte) 13, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ HOOK_CHIP_ID, "hook", ChipType.TACTIC,
            /*maxUses*/ -1));

        // 1008 "laser": damage over LASER_LINE — line launch type, so casting
        // at a non-aligned enemy fails silently (no RNG drawn); when aligned
        // the area covers every cell along the ray (obstacles break it).
        var laserEffects = Json.createArray();
        var laserDmg = Json.createObject();
        laserDmg.put("id", 1); // EFFECT_DAMAGE
        laserDmg.put("value1", 8);
        laserDmg.put("value2", 2);
        laserDmg.put("turns", 0);
        laserDmg.put("targets", 31);
        laserDmg.put("modifiers", 0);
        laserEffects.add(laserDmg);
        Chips.addChip(new Chip(
            LASER_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, laserEffects,
            /*launchType*/ (byte) 1, /*area*/ (byte) 2, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ LASER_CHIP_ID, "laser", ChipType.DAMAGE,
            /*maxUses*/ -1));

        // 1009 "storm": self-cast damage over the ENEMIES area — every enemy
        // is hit wherever it stands (full area factor, no LoS).
        var stormEffects = Json.createArray();
        var stormDmg = Json.createObject();
        stormDmg.put("id", 1); // EFFECT_DAMAGE
        stormDmg.put("value1", 4);
        stormDmg.put("value2", 2);
        stormDmg.put("turns", 0);
        stormDmg.put("targets", 31);
        stormDmg.put("modifiers", 0);
        stormEffects.add(stormDmg);
        Chips.addChip(new Chip(
            STORM_CHIP_ID, /*cost*/ 2, /*minRange*/ 0, /*maxRange*/ 0, stormEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 14, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ STORM_CHIP_ID, "storm", ChipType.DAMAGE,
            /*maxUses*/ -1));

        // 1010 "blessing": self-cast non-stackable strength buff over the
        // ALLIES area — recasts replace the previous effect (ActionUpdate).
        var blessingEffects = Json.createArray();
        var blessingBuff = Json.createObject();
        blessingBuff.put("id", 3); // EFFECT_BUFF_STRENGTH
        blessingBuff.put("value1", 4);
        blessingBuff.put("value2", 2);
        blessingBuff.put("turns", 2);
        blessingBuff.put("targets", 31);
        blessingBuff.put("modifiers", 0);
        blessingEffects.add(blessingBuff);
        Chips.addChip(new Chip(
            BLESSING_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, blessingEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 15, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ BLESSING_CHIP_ID, "blessing", ChipType.BOOST,
            /*maxUses*/ -1));

        // 1011 "cure": instant heal (turns 0) 12+4×jet, wisdom-scaled, capped
        // to the missing life; ActionHeal is logged even for a 0 heal.
        var cureEffects = Json.createArray();
        var cureHeal = Json.createObject();
        cureHeal.put("id", 2); // EFFECT_HEAL
        cureHeal.put("value1", 12);
        cureHeal.put("value2", 4);
        cureHeal.put("turns", 0);
        cureHeal.put("targets", 31);
        cureHeal.put("modifiers", 0);
        cureEffects.add(cureHeal);
        Chips.addChip(new Chip(
            CURE_CHIP_ID, /*cost*/ 2, /*minRange*/ 0, /*maxRange*/ 6, cureEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ CURE_CHIP_ID, "cure", ChipType.HEAL,
            /*maxUses*/ -1));

        // 1012 "regen": heal-over-time 4+2×jet for 3 turns, self-cast,
        // cooldown 2 — the recast lands while the previous effect is still
        // live, exercising remove-previous on a heal.
        var regenEffects = Json.createArray();
        var regenHeal = Json.createObject();
        regenHeal.put("id", 2); // EFFECT_HEAL
        regenHeal.put("value1", 4);
        regenHeal.put("value2", 2);
        regenHeal.put("turns", 3);
        regenHeal.put("targets", 31);
        regenHeal.put("modifiers", 0);
        regenEffects.add(regenHeal);
        Chips.addChip(new Chip(
            REGEN_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, regenEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 2, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ REGEN_CHIP_ID, "regen", ChipType.HEAL,
            /*maxUses*/ -1));

        // 1013 "wall": absolute shield 6+2×jet for 2 turns, resistance-scaled,
        // self-cast, non-stackable — recasts churn through remove-previous.
        var wallEffects = Json.createArray();
        var wallShield = Json.createObject();
        wallShield.put("id", 6); // EFFECT_ABSOLUTE_SHIELD
        wallShield.put("value1", 6);
        wallShield.put("value2", 2);
        wallShield.put("turns", 2);
        wallShield.put("targets", 31);
        wallShield.put("modifiers", 0);
        wallEffects.add(wallShield);
        Chips.addChip(new Chip(
            WALL_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, wallEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ WALL_CHIP_ID, "wall", ChipType.PROTECTION,
            /*maxUses*/ -1));

        // 1014 "mirror": damage return 8+2×jet for 2 turns, agility-scaled,
        // self-cast — attackers take return damage, which can kill them
        // mid-turn (the ENTITY_DIED silent turn abort).
        var mirrorEffects = Json.createArray();
        var mirrorReturn = Json.createObject();
        mirrorReturn.put("id", 20); // EFFECT_DAMAGE_RETURN
        mirrorReturn.put("value1", 8);
        mirrorReturn.put("value2", 2);
        mirrorReturn.put("turns", 2);
        mirrorReturn.put("targets", 31);
        mirrorReturn.put("modifiers", 0);
        mirrorEffects.add(mirrorReturn);
        Chips.addChip(new Chip(
            MIRROR_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, mirrorEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ MIRROR_CHIP_ID, "mirror", ChipType.RETURN,
            /*maxUses*/ -1));

        // 1015 "ice": shackle MP 2+1×jet for 2 turns, magic-scaled, carried as
        // a negative MP stat — the shackled enemy's pathing visibly changes.
        var iceEffects = Json.createArray();
        var iceShackle = Json.createObject();
        iceShackle.put("id", 17); // EFFECT_SHACKLE_MP
        iceShackle.put("value1", 2);
        iceShackle.put("value2", 1);
        iceShackle.put("turns", 2);
        iceShackle.put("targets", 31);
        iceShackle.put("modifiers", 0);
        iceEffects.add(iceShackle);
        Chips.addChip(new Chip(
            ICE_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, iceEffects,
            /*launchType*/ (byte) 1, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ ICE_CHIP_ID, "ice", ChipType.SHACKLE,
            /*maxUses*/ -1));

        // 1016 "mud": shackle TP 1+1×jet for 2 turns — the shackled enemy
        // loses casts (remaining TP drops immediately).
        var mudEffects = Json.createArray();
        var mudShackle = Json.createObject();
        mudShackle.put("id", 18); // EFFECT_SHACKLE_TP
        mudShackle.put("value1", 1);
        mudShackle.put("value2", 1);
        mudShackle.put("turns", 2);
        mudShackle.put("targets", 31);
        mudShackle.put("modifiers", 0);
        mudEffects.add(mudShackle);
        Chips.addChip(new Chip(
            MUD_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, mudEffects,
            /*launchType*/ (byte) 1, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ MUD_CHIP_ID, "mud", ChipType.SHACKLE,
            /*maxUses*/ -1));

        // 1017 "armor": relative shield 10+5×jet for 2 turns, resistance-
        // scaled, self-cast.
        var armorEffects = Json.createArray();
        var armorShield = Json.createObject();
        armorShield.put("id", 5); // EFFECT_RELATIVE_SHIELD
        armorShield.put("value1", 10);
        armorShield.put("value2", 5);
        armorShield.put("turns", 2);
        armorShield.put("targets", 31);
        armorShield.put("modifiers", 0);
        armorEffects.add(armorShield);
        Chips.addChip(new Chip(
            ARMOR_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, armorEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ ARMOR_CHIP_ID, "armor", ChipType.PROTECTION,
            /*maxUses*/ -1));

        // 1018 "swap": PERMUTATION — cast at the enemy's (occupied) cell, the
        // caster and the target trade places. Log-silent like the slides;
        // observable through every later path/range.
        var swapEffects = Json.createArray();
        var swapPerm = Json.createObject();
        swapPerm.put("id", 11); // EFFECT_PERMUTATION
        swapPerm.put("value1", 0);
        swapPerm.put("value2", 0);
        swapPerm.put("turns", 0);
        swapPerm.put("targets", 31);
        swapPerm.put("modifiers", 0);
        swapEffects.add(swapPerm);
        Chips.addChip(new Chip(
            SWAP_CHIP_ID, /*cost*/ 1, /*minRange*/ 1, /*maxRange*/ 10, swapEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ SWAP_CHIP_ID, "swap", ChipType.TACTIC,
            /*maxUses*/ -1));

        // 1019 "spring": [repel, damage 6+2×jet] — EffectRepel is an EMPTY
        // class in this generator (no apply, no Attack.java pre-block), so
        // the repel line is dead and only the damage lands. Pins that.
        var springEffects = Json.createArray();
        var springRepel = Json.createObject();
        springRepel.put("id", 53); // EFFECT_REPEL
        springRepel.put("value1", 0);
        springRepel.put("value2", 0);
        springRepel.put("turns", 0);
        springRepel.put("targets", 31);
        springRepel.put("modifiers", 0);
        springEffects.add(springRepel);
        var springDmg = Json.createObject();
        springDmg.put("id", 1); // EFFECT_DAMAGE
        springDmg.put("value1", 6);
        springDmg.put("value2", 2);
        springDmg.put("turns", 0);
        springDmg.put("targets", 31);
        springDmg.put("modifiers", 0);
        springEffects.add(springDmg);
        Chips.addChip(new Chip(
            SPRING_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 7, springEffects,
            /*launchType*/ (byte) 1, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ SPRING_CHIP_ID, "spring", ChipType.DAMAGE,
            /*maxUses*/ -1));

        // 1020 "fortress": VITALITY 30+10×jet for 2 turns, self-cast —
        // wisdom-scaled max-life increase + heal; ActionVitality logged
        // unconditionally; the totalLife bump is PERMANENT (expiry removes
        // the effect but reverts nothing — the stored effect has no stats).
        var fortressEffects = Json.createArray();
        var fortressVita = Json.createObject();
        fortressVita.put("id", 12); // EFFECT_VITALITY
        fortressVita.put("value1", 30);
        fortressVita.put("value2", 10);
        fortressVita.put("turns", 2);
        fortressVita.put("targets", 31);
        fortressVita.put("modifiers", 0);
        fortressEffects.add(fortressVita);
        Chips.addChip(new Chip(
            FORTRESS_CHIP_ID, /*cost*/ 2, /*minRange*/ 0, /*maxRange*/ 0, fortressEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ FORTRESS_CHIP_ID, "fortress", ChipType.BOOST,
            /*maxUses*/ -1));

        // 1021 "reflex": BUFF_AGILITY 25+10×jet for 2 turns, self-cast —
        // agility feeds the crit roll AND the damage-return formula.
        var reflexEffects = Json.createArray();
        var reflexBuff = Json.createObject();
        reflexBuff.put("id", 4); // EFFECT_BUFF_AGILITY
        reflexBuff.put("value1", 25);
        reflexBuff.put("value2", 10);
        reflexBuff.put("turns", 2);
        reflexBuff.put("targets", 31);
        reflexBuff.put("modifiers", 0);
        reflexEffects.add(reflexBuff);
        Chips.addChip(new Chip(
            REFLEX_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, reflexEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ REFLEX_CHIP_ID, "reflex", ChipType.BOOST,
            /*maxUses*/ -1));

        // 1022 "haste": BUFF_MP 2+1×jet for 2 turns, self-cast.
        var hasteEffects = Json.createArray();
        var hasteBuff = Json.createObject();
        hasteBuff.put("id", 7); // EFFECT_BUFF_MP
        hasteBuff.put("value1", 2);
        hasteBuff.put("value2", 1);
        hasteBuff.put("turns", 2);
        hasteBuff.put("targets", 31);
        hasteBuff.put("modifiers", 0);
        hasteEffects.add(hasteBuff);
        Chips.addChip(new Chip(
            HASTE_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, hasteEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ HASTE_CHIP_ID, "haste", ChipType.BOOST,
            /*maxUses*/ -1));

        // 1023 "focus": BUFF_TP 1+1×jet for 2 turns, self-cast — the extra TP
        // is usable immediately (getTP = total + buffs − used), making the
        // rest of the turn's cast ladder jet-sensitive.
        var focusEffects = Json.createArray();
        var focusBuff = Json.createObject();
        focusBuff.put("id", 8); // EFFECT_BUFF_TP
        focusBuff.put("value1", 1);
        focusBuff.put("value2", 1);
        focusBuff.put("turns", 2);
        focusBuff.put("targets", 31);
        focusBuff.put("modifiers", 0);
        focusEffects.add(focusBuff);
        Chips.addChip(new Chip(
            FOCUS_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, focusEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ FOCUS_CHIP_ID, "focus", ChipType.BOOST,
            /*maxUses*/ -1));

        // 1024 "sage": BUFF_WISDOM 20+5×jet for 2 turns, self-cast — wisdom
        // feeds life-steal on later damage and heal values.
        var sageEffects = Json.createArray();
        var sageBuff = Json.createObject();
        sageBuff.put("id", 22); // EFFECT_BUFF_WISDOM
        sageBuff.put("value1", 20);
        sageBuff.put("value2", 5);
        sageBuff.put("turns", 2);
        sageBuff.put("targets", 31);
        sageBuff.put("modifiers", 0);
        sageEffects.add(sageBuff);
        Chips.addChip(new Chip(
            SAGE_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, sageEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ SAGE_CHIP_ID, "sage", ChipType.BOOST,
            /*maxUses*/ -1));

        // 1025 "brick": BUFF_RESISTANCE 15+5×jet for 2 turns, self-cast.
        var brickEffects = Json.createArray();
        var brickBuff = Json.createObject();
        brickBuff.put("id", 21); // EFFECT_BUFF_RESISTANCE
        brickBuff.put("value1", 15);
        brickBuff.put("value2", 5);
        brickBuff.put("turns", 2);
        brickBuff.put("targets", 31);
        brickBuff.put("modifiers", 0);
        brickEffects.add(brickBuff);
        Chips.addChip(new Chip(
            BRICK_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, brickEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ BRICK_CHIP_ID, "brick", ChipType.BOOST,
            /*maxUses*/ -1));

        // 1026 "weaken": SHACKLE_STRENGTH 10+3×jet for 2 turns, range 1–8 —
        // the shackled strength shows in the target's later damage rolls.
        var weakenEffects = Json.createArray();
        var weakenShackle = Json.createObject();
        weakenShackle.put("id", 19); // EFFECT_SHACKLE_STRENGTH
        weakenShackle.put("value1", 10);
        weakenShackle.put("value2", 3);
        weakenShackle.put("turns", 2);
        weakenShackle.put("targets", 31);
        weakenShackle.put("modifiers", 0);
        weakenEffects.add(weakenShackle);
        Chips.addChip(new Chip(
            WEAKEN_CHIP_ID, /*cost*/ 1, /*minRange*/ 1, /*maxRange*/ 8, weakenEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ WEAKEN_CHIP_ID, "weaken", ChipType.SHACKLE,
            /*maxUses*/ -1));

        // 1027 "numb": SHACKLE_AGILITY 10+3×jet for 2 turns, range 1–8 —
        // shackled agility cuts the target's crit chance.
        var numbEffects = Json.createArray();
        var numbShackle = Json.createObject();
        numbShackle.put("id", 47); // EFFECT_SHACKLE_AGILITY
        numbShackle.put("value1", 10);
        numbShackle.put("value2", 3);
        numbShackle.put("turns", 2);
        numbShackle.put("targets", 31);
        numbShackle.put("modifiers", 0);
        numbEffects.add(numbShackle);
        Chips.addChip(new Chip(
            NUMB_CHIP_ID, /*cost*/ 1, /*minRange*/ 1, /*maxRange*/ 8, numbEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ NUMB_CHIP_ID, "numb", ChipType.SHACKLE,
            /*maxUses*/ -1));

        // 1028 "dull": SHACKLE_WISDOM 8+2×jet for 2 turns, range 1–8 —
        // shackled wisdom cuts the target's life-steal and heals.
        var dullEffects = Json.createArray();
        var dullShackle = Json.createObject();
        dullShackle.put("id", 48); // EFFECT_SHACKLE_WISDOM
        dullShackle.put("value1", 8);
        dullShackle.put("value2", 2);
        dullShackle.put("turns", 2);
        dullShackle.put("targets", 31);
        dullShackle.put("modifiers", 0);
        dullEffects.add(dullShackle);
        Chips.addChip(new Chip(
            DULL_CHIP_ID, /*cost*/ 1, /*minRange*/ 1, /*maxRange*/ 8, dullEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ DULL_CHIP_ID, "dull", ChipType.SHACKLE,
            /*maxUses*/ -1));

        // 1029 "hush": SHACKLE_MAGIC 5+2×jet for 2 turns, range 1–8 — the
        // stored value pins the formula even with no magic-dependent
        // downstream path (our leeks have 0 magic).
        var hushEffects = Json.createArray();
        var hushShackle = Json.createObject();
        hushShackle.put("id", 24); // EFFECT_SHACKLE_MAGIC
        hushShackle.put("value1", 5);
        hushShackle.put("value2", 2);
        hushShackle.put("turns", 2);
        hushShackle.put("targets", 31);
        hushShackle.put("modifiers", 0);
        hushEffects.add(hushShackle);
        Chips.addChip(new Chip(
            HUSH_CHIP_ID, /*cost*/ 1, /*minRange*/ 1, /*maxRange*/ 8, hushEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ HUSH_CHIP_ID, "hush", ChipType.SHACKLE,
            /*maxUses*/ -1));

        // 1030 "cleanse": [ANTIDOTE, REMOVE_SHACKLES] instant, self-cast —
        // each removed effect logs `[303]`, then `[307]`/`[308]` land.
        var cleanseEffects = Json.createArray();
        var antidote = Json.createObject();
        antidote.put("id", 23); // EFFECT_ANTIDOTE
        antidote.put("value1", 0);
        antidote.put("value2", 0);
        antidote.put("turns", 0);
        antidote.put("targets", 31);
        antidote.put("modifiers", 0);
        cleanseEffects.add(antidote);
        var unshackle = Json.createObject();
        unshackle.put("id", 49); // EFFECT_REMOVE_SHACKLES
        unshackle.put("value1", 0);
        unshackle.put("value2", 0);
        unshackle.put("turns", 0);
        unshackle.put("targets", 31);
        unshackle.put("modifiers", 0);
        cleanseEffects.add(unshackle);
        Chips.addChip(new Chip(
            CLEANSE_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, cleanseEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ CLEANSE_CHIP_ID, "cleanse", ChipType.TACTIC,
            /*maxUses*/ -1));

        // 1031 "unravel": DEBUFF 40+10×jet percent, instant, range 1–8 — the
        // value uses a TRUNCATING (int) cast (not Math.round); reduces every
        // non-IRREDUCTIBLE effect on the target ([304] updates / [303]
        // removals), then logs [306].
        var unravelEffects = Json.createArray();
        var debuff = Json.createObject();
        debuff.put("id", 9); // EFFECT_DEBUFF
        debuff.put("value1", 40);
        debuff.put("value2", 10);
        debuff.put("turns", 0);
        debuff.put("targets", 31);
        debuff.put("modifiers", 0);
        unravelEffects.add(debuff);
        Chips.addChip(new Chip(
            UNRAVEL_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, unravelEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ UNRAVEL_CHIP_ID, "unravel", ChipType.TACTIC,
            /*maxUses*/ -1));

        // 1032 "javelin": DAMAGE 5+2×jet, launch type 9, range 2–7, LoS —
        // range-gates exactly like LAUNCH_TYPE_LINE, but cast-cell search
        // (getCellToUseChip) goes through the generateMask path instead of
        // the line-walking branch.
        var javelinEffects = Json.createArray();
        var javelinDamage = Json.createObject();
        javelinDamage.put("id", 1); // EFFECT_DAMAGE
        javelinDamage.put("value1", 5);
        javelinDamage.put("value2", 2);
        javelinDamage.put("turns", 0);
        javelinDamage.put("targets", 31);
        javelinDamage.put("modifiers", 0);
        javelinEffects.add(javelinDamage);
        Chips.addChip(new Chip(
            JAVELIN_CHIP_ID, /*cost*/ 2, /*minRange*/ 2, /*maxRange*/ 7, javelinEffects,
            /*launchType*/ (byte) 9, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ JAVELIN_CHIP_ID, "javelin", ChipType.DAMAGE,
            /*maxUses*/ -1));

        // 1033 "comet": DAMAGE 5+2×jet, launch type 10, range 1–8, no LoS —
        // diagonal-only casts with the len=max mask special case.
        var cometEffects = Json.createArray();
        var cometDamage = Json.createObject();
        cometDamage.put("id", 1); // EFFECT_DAMAGE
        cometDamage.put("value1", 5);
        cometDamage.put("value2", 2);
        cometDamage.put("turns", 0);
        cometDamage.put("targets", 31);
        cometDamage.put("modifiers", 0);
        cometEffects.add(cometDamage);
        Chips.addChip(new Chip(
            COMET_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, cometEffects,
            /*launchType*/ (byte) 10, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ COMET_CHIP_ID, "comet", ChipType.DAMAGE,
            /*maxUses*/ -1));

        // 1034 "statue": ADD_STATE STATIC (ordinal 11) for 2 turns, range
        // 1–8 — the target can't move (no log, no MP), can't be pushed or
        // attracted, and can't be the TARGET of a permutation (but a static
        // CASTER still swaps — Java only checks the target).
        var statueEffects = Json.createArray();
        var statueState = Json.createObject();
        statueState.put("id", 59); // EFFECT_ADD_STATE
        statueState.put("value1", 11); // EntityState.STATIC
        statueState.put("value2", 0);
        statueState.put("turns", 2);
        statueState.put("targets", 31);
        statueState.put("modifiers", 0);
        statueEffects.add(statueState);
        Chips.addChip(new Chip(
            STATUE_CHIP_ID, /*cost*/ 1, /*minRange*/ 1, /*maxRange*/ 8, statueEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ STATUE_CHIP_ID, "statue", ChipType.TACTIC,
            /*maxUses*/ -1));

        // 1035 "ghost": ADD_STATE INVINCIBLE (ordinal 3) for 2 turns, self —
        // incoming damage zeroes AFTER shields but still logs [101, fid, 0,
        // 0]; poison ticks zero and log NOTHING; return damage to an
        // invincible attacker is skipped; but damage return *from* an
        // invincible target still flows (computed from the raw pre-shield
        // value).
        var ghostEffects = Json.createArray();
        var ghostState = Json.createObject();
        ghostState.put("id", 59); // EFFECT_ADD_STATE
        ghostState.put("value1", 3); // EntityState.INVINCIBLE
        ghostState.put("value2", 0);
        ghostState.put("turns", 2);
        ghostState.put("targets", 31);
        ghostState.put("modifiers", 0);
        ghostEffects.add(ghostState);
        Chips.addChip(new Chip(
            GHOST_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, ghostEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ GHOST_CHIP_ID, "ghost", ChipType.TACTIC,
            /*maxUses*/ -1));

        // 1036 "curse": ADD_STATE UNHEALABLE (ordinal 2) for 2 turns, range
        // 1–8 — instant heals return BEFORE the log (no 0-heal action,
        // unlike the full-life cap), HoT ticks skip silently, and the cursed
        // entity's own life-steal is blocked.
        var curseEffects = Json.createArray();
        var curseState = Json.createObject();
        curseState.put("id", 59); // EFFECT_ADD_STATE
        curseState.put("value1", 2); // EntityState.UNHEALABLE
        curseState.put("value2", 0);
        curseState.put("turns", 2);
        curseState.put("targets", 31);
        curseState.put("modifiers", 0);
        curseEffects.add(curseState);
        Chips.addChip(new Chip(
            CURSE_CHIP_ID, /*cost*/ 1, /*minRange*/ 1, /*maxRange*/ 8, curseEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ CURSE_CHIP_ID, "curse", ChipType.TACTIC,
            /*maxUses*/ -1));

        // 1037 "toxin": AFTEREFFECT 8+3×jet for 2 turns (science-scaled
        // damage applied at cast AND re-ticked at each of the target's turn
        // starts — the tick re-clamps to the target's life, persists the
        // clamp, has NO invincible check and logs even a 0 tick), paired
        // with an ALLY_KILLED_TO_AGILITY line — a DEAD effect (empty class
        // upstream): it moves nothing, logs nothing, stores nothing.
        var toxinEffects = Json.createArray();
        var aftereffect = Json.createObject();
        aftereffect.put("id", 25); // EFFECT_AFTEREFFECT
        aftereffect.put("value1", 8);
        aftereffect.put("value2", 3);
        aftereffect.put("turns", 2);
        aftereffect.put("targets", 31);
        aftereffect.put("modifiers", 0);
        toxinEffects.add(aftereffect);
        var deadLine = Json.createObject();
        deadLine.put("id", 55); // EFFECT_ALLY_KILLED_TO_AGILITY (dead)
        deadLine.put("value1", 100);
        deadLine.put("value2", 0);
        deadLine.put("turns", 0);
        deadLine.put("targets", 31);
        deadLine.put("modifiers", 0);
        toxinEffects.add(deadLine);
        Chips.addChip(new Chip(
            TOXIN_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, toxinEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ TOXIN_CHIP_ID, "toxin", ChipType.POISON,
            /*maxUses*/ -1));

        // 1038 "reaper": [damage 8+2×jet, STEAL_LIFE on-caster] — the steal
        // line's value is previousEffectTotalValue (the damage line's total),
        // healing the caster (UNHEALABLE-guarded, capped, logs ActionHeal).
        var reaperEffects = Json.createArray();
        var reaperDamage = Json.createObject();
        reaperDamage.put("id", 1); // EFFECT_DAMAGE
        reaperDamage.put("value1", 8);
        reaperDamage.put("value2", 2);
        reaperDamage.put("turns", 0);
        reaperDamage.put("targets", 31);
        reaperDamage.put("modifiers", 0);
        reaperEffects.add(reaperDamage);
        var stealLife = Json.createObject();
        stealLife.put("id", 61); // EFFECT_STEAL_LIFE
        stealLife.put("value1", 0);
        stealLife.put("value2", 0);
        stealLife.put("turns", 0);
        stealLife.put("targets", 31);
        stealLife.put("modifiers", 4); // ON_CASTER
        reaperEffects.add(stealLife);
        Chips.addChip(new Chip(
            REAPER_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 7, reaperEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ REAPER_CHIP_ID, "reaper", ChipType.DAMAGE,
            /*maxUses*/ -1));

        // 1039 "leech": [damage 6+2×jet, STEAL_ABSOLUTE_SHIELD on-caster for
        // 2 turns] — carries the damage line's total value as an absolute
        // shield stat on the caster.
        var leechEffects = Json.createArray();
        var leechDamage = Json.createObject();
        leechDamage.put("id", 1); // EFFECT_DAMAGE
        leechDamage.put("value1", 6);
        leechDamage.put("value2", 2);
        leechDamage.put("turns", 0);
        leechDamage.put("targets", 31);
        leechDamage.put("modifiers", 0);
        leechEffects.add(leechDamage);
        var stealShield = Json.createObject();
        stealShield.put("id", 29); // EFFECT_STEAL_ABSOLUTE_SHIELD
        stealShield.put("value1", 0);
        stealShield.put("value2", 0);
        stealShield.put("turns", 2);
        stealShield.put("targets", 31);
        stealShield.put("modifiers", 4); // ON_CASTER
        leechEffects.add(stealShield);
        Chips.addChip(new Chip(
            LEECH_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 7, leechEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ LEECH_CHIP_ID, "leech", ChipType.DAMAGE,
            /*maxUses*/ -1));

        // 1040 "cataclysm": [NOVA_DAMAGE 10+4×jet, LIFE_DAMAGE 4+2×jet] —
        // nova is pure erosion (clamped to the missing life, removeLife(0,
        // value), logged [107, fid, value, 0]); life damage scales with the
        // CASTER's current life, takes shields and return damage but zeroes
        // for invincible targets BEFORE the return is computed and steals
        // no life.
        var cataclysmEffects = Json.createArray();
        var nova = Json.createObject();
        nova.put("id", 30); // EFFECT_NOVA_DAMAGE
        nova.put("value1", 10);
        nova.put("value2", 4);
        nova.put("turns", 0);
        nova.put("targets", 31);
        nova.put("modifiers", 0);
        cataclysmEffects.add(nova);
        var lifeDamage = Json.createObject();
        lifeDamage.put("id", 28); // EFFECT_LIFE_DAMAGE
        lifeDamage.put("value1", 4);
        lifeDamage.put("value2", 2);
        lifeDamage.put("turns", 0);
        lifeDamage.put("targets", 31);
        lifeDamage.put("modifiers", 0);
        cataclysmEffects.add(lifeDamage);
        Chips.addChip(new Chip(
            CATACLYSM_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, cataclysmEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ CATACLYSM_CHIP_ID, "cataclysm", ChipType.DAMAGE,
            /*maxUses*/ -1));

        // 1041 "doom": KILL — value = the target's full life, ActionKill
        // logs the TARGET fid in both fields (upstream ctor bug), and the
        // invincible check is commented out upstream ("// Graal").
        var doomEffects = Json.createArray();
        var kill = Json.createObject();
        kill.put("id", 16); // EFFECT_KILL
        kill.put("value1", 0);
        kill.put("value2", 0);
        kill.put("turns", 0);
        kill.put("targets", 31);
        kill.put("modifiers", 0);
        doomEffects.add(kill);
        Chips.addChip(new Chip(
            DOOM_CHIP_ID, /*cost*/ 4, /*minRange*/ 1, /*maxRange*/ 6, doomEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ DOOM_CHIP_ID, "doom", ChipType.DAMAGE,
            /*maxUses*/ -1));

        // 1042 "mutation": raw stat buffs for 2 turns, self-cast — STR and
        // AGI are IRREDUCTIBLE (a debuff skips them, a TOTAL_DEBUFF does
        // not), POWER and MAGIC feed later damage/poison/shackle scaling.
        var mutationEffects = Json.createArray();
        var rawStr = Json.createObject();
        rawStr.put("id", 38); // EFFECT_RAW_BUFF_STRENGTH
        rawStr.put("value1", 10);
        rawStr.put("value2", 5);
        rawStr.put("turns", 2);
        rawStr.put("targets", 31);
        rawStr.put("modifiers", 16); // IRREDUCTIBLE
        mutationEffects.add(rawStr);
        var rawAgi = Json.createObject();
        rawAgi.put("id", 41); // EFFECT_RAW_BUFF_AGILITY
        rawAgi.put("value1", 10);
        rawAgi.put("value2", 5);
        rawAgi.put("turns", 2);
        rawAgi.put("targets", 31);
        rawAgi.put("modifiers", 16); // IRREDUCTIBLE
        mutationEffects.add(rawAgi);
        var rawPower = Json.createObject();
        rawPower.put("id", 52); // EFFECT_RAW_BUFF_POWER
        rawPower.put("value1", 5);
        rawPower.put("value2", 2);
        rawPower.put("turns", 2);
        rawPower.put("targets", 31);
        rawPower.put("modifiers", 0);
        mutationEffects.add(rawPower);
        var rawMagic = Json.createObject();
        rawMagic.put("id", 39); // EFFECT_RAW_BUFF_MAGIC
        rawMagic.put("value1", 10);
        rawMagic.put("value2", 5);
        rawMagic.put("turns", 2);
        rawMagic.put("targets", 31);
        rawMagic.put("modifiers", 0);
        mutationEffects.add(rawMagic);
        Chips.addChip(new Chip(
            MUTATION_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, mutationEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ MUTATION_CHIP_ID, "mutation", ChipType.BOOST,
            /*maxUses*/ -1));

        // 1043 "clarity": raw SCIENCE/WISDOM/RESISTANCE buffs for 2 turns,
        // self-cast — the science buff makes later science-scaled casts
        // (aftereffect, nova, regular buffs) actually scale.
        var clarityEffects = Json.createArray();
        var rawSci = Json.createObject();
        rawSci.put("id", 40); // EFFECT_RAW_BUFF_SCIENCE
        rawSci.put("value1", 20);
        rawSci.put("value2", 10);
        rawSci.put("turns", 2);
        rawSci.put("targets", 31);
        rawSci.put("modifiers", 0);
        clarityEffects.add(rawSci);
        var rawWis = Json.createObject();
        rawWis.put("id", 44); // EFFECT_RAW_BUFF_WISDOM
        rawWis.put("value1", 10);
        rawWis.put("value2", 5);
        rawWis.put("turns", 2);
        rawWis.put("targets", 31);
        rawWis.put("modifiers", 0);
        clarityEffects.add(rawWis);
        var rawRes = Json.createObject();
        rawRes.put("id", 42); // EFFECT_RAW_BUFF_RESISTANCE
        rawRes.put("value1", 10);
        rawRes.put("value2", 5);
        rawRes.put("turns", 2);
        rawRes.put("targets", 31);
        rawRes.put("modifiers", 0);
        clarityEffects.add(rawRes);
        Chips.addChip(new Chip(
            CLARITY_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, clarityEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ CLARITY_CHIP_ID, "clarity", ChipType.BOOST,
            /*maxUses*/ -1));

        // 1044 "bulwark": raw shields + RAW_BUFF_MP/TP for 2 turns, self —
        // the MP/TP lines use the targetCount-shaped formula (×targetCount,
        // NO aoe factor) and land immediately (visible movement/casts).
        var bulwarkEffects = Json.createArray();
        var rawAbs = Json.createObject();
        rawAbs.put("id", 37); // EFFECT_RAW_ABSOLUTE_SHIELD
        rawAbs.put("value1", 8);
        rawAbs.put("value2", 4);
        rawAbs.put("turns", 2);
        rawAbs.put("targets", 31);
        rawAbs.put("modifiers", 0);
        bulwarkEffects.add(rawAbs);
        var rawRel = Json.createObject();
        rawRel.put("id", 54); // EFFECT_RAW_RELATIVE_SHIELD
        rawRel.put("value1", 8);
        rawRel.put("value2", 4);
        rawRel.put("turns", 2);
        rawRel.put("targets", 31);
        rawRel.put("modifiers", 0);
        bulwarkEffects.add(rawRel);
        var rawMp = Json.createObject();
        rawMp.put("id", 31); // EFFECT_RAW_BUFF_MP
        rawMp.put("value1", 1);
        rawMp.put("value2", 1);
        rawMp.put("turns", 2);
        rawMp.put("targets", 31);
        rawMp.put("modifiers", 0);
        bulwarkEffects.add(rawMp);
        var rawTp = Json.createObject();
        rawTp.put("id", 32); // EFFECT_RAW_BUFF_TP
        rawTp.put("value1", 1);
        rawTp.put("value2", 1);
        rawTp.put("turns", 2);
        rawTp.put("targets", 31);
        rawTp.put("modifiers", 0);
        bulwarkEffects.add(rawTp);
        Chips.addChip(new Chip(
            BULWARK_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, bulwarkEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ BULWARK_CHIP_ID, "bulwark", ChipType.PROTECTION,
            /*maxUses*/ -1));

        // 1045 "rupture": [VULNERABILITY 15+5×jet, ABSOLUTE_VULNERABILITY
        // 10+5×jet] for 2 turns — unscaled NEGATIVE shield carriers, so
        // later damage on the target is amplified.
        var ruptureEffects = Json.createArray();
        var vuln = Json.createObject();
        vuln.put("id", 26); // EFFECT_VULNERABILITY
        vuln.put("value1", 15);
        vuln.put("value2", 5);
        vuln.put("turns", 2);
        vuln.put("targets", 31);
        vuln.put("modifiers", 0);
        ruptureEffects.add(vuln);
        var absVuln = Json.createObject();
        absVuln.put("id", 27); // EFFECT_ABSOLUTE_VULNERABILITY
        absVuln.put("value1", 10);
        absVuln.put("value2", 5);
        absVuln.put("turns", 2);
        absVuln.put("targets", 31);
        absVuln.put("modifiers", 0);
        ruptureEffects.add(absVuln);
        Chips.addChip(new Chip(
            RUPTURE_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, ruptureEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ RUPTURE_CHIP_ID, "rupture", ChipType.SHACKLE,
            /*maxUses*/ -1));

        // 1046 "purge": TOTAL_DEBUFF 30+20×jet percent — truncating (int)
        // cast like debuff, but reduceEffectsTotal does NOT skip
        // IRREDUCTIBLE effects (mutation's STR/AGI lines shrink too).
        var purgeEffects = Json.createArray();
        var totalDebuff = Json.createObject();
        totalDebuff.put("id", 60); // EFFECT_TOTAL_DEBUFF
        totalDebuff.put("value1", 30);
        totalDebuff.put("value2", 20);
        totalDebuff.put("turns", 0);
        totalDebuff.put("targets", 31);
        totalDebuff.put("modifiers", 0);
        purgeEffects.add(totalDebuff);
        Chips.addChip(new Chip(
            PURGE_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, purgeEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ PURGE_CHIP_ID, "purge", ChipType.SHACKLE,
            /*maxUses*/ -1));

        // 1047 "transfusion": [NOVA_VITALITY 15+5×jet, RAW_HEAL 10+5×jet],
        // self — nova vitality bumps the max life WITHOUT healing (science-
        // scaled, no floor, no invincible check, [112, fid, value]), then
        // the raw heal (no wisdom, no floor, logs even 0) fills the new
        // headroom.
        var transfusionEffects = Json.createArray();
        var novaVit = Json.createObject();
        novaVit.put("id", 45); // EFFECT_NOVA_VITALITY
        novaVit.put("value1", 15);
        novaVit.put("value2", 5);
        novaVit.put("turns", 0);
        novaVit.put("targets", 31);
        novaVit.put("modifiers", 0);
        transfusionEffects.add(novaVit);
        var rawHeal = Json.createObject();
        rawHeal.put("id", 57); // EFFECT_RAW_HEAL
        rawHeal.put("value1", 10);
        rawHeal.put("value2", 5);
        rawHeal.put("turns", 0);
        rawHeal.put("targets", 31);
        rawHeal.put("modifiers", 0);
        transfusionEffects.add(rawHeal);
        Chips.addChip(new Chip(
            TRANSFUSION_CHIP_ID, /*cost*/ 1, /*minRange*/ 0, /*maxRange*/ 0, transfusionEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 0, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ TRANSFUSION_CHIP_ID, "transfusion", ChipType.HEAL,
            /*maxUses*/ -1));

        // 1048 "spawn": TYPE_SUMMON → bulb template 1001, range 1–8 free
        // launch with LoS, TEAM cooldown 2 — exercises the summonEntity
        // ladder (UseChip + Invocation logs, exactly one crit getDouble on
        // success, failures draw NO RNG, no addItemUse) and the useChip
        // intercept (BULB_WITHOUT_AI idle bulbs).
        var spawnEffects = Json.createArray();
        var summonEffect = Json.createObject();
        summonEffect.put("id", 14); // EFFECT_SUMMON (EffectSummon is empty)
        summonEffect.put("value1", HARNESS_BULB_ID);
        summonEffect.put("value2", 0);
        summonEffect.put("turns", 0);
        summonEffect.put("targets", 31);
        summonEffect.put("modifiers", 0);
        spawnEffects.add(summonEffect);
        Chips.addChip(new Chip(
            SPAWN_CHIP_ID, /*cost*/ 2, /*minRange*/ 1, /*maxRange*/ 8, spawnEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ true,
            /*cooldown*/ 2, /*teamCooldown*/ true, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ SPAWN_CHIP_ID, "spawn", ChipType.BULB,
            /*maxUses*/ -1));

        // 84 "revive": TYPE_RESURRECT, range 1–8 free launch without LoS,
        // 3 TP, cooldown 4 — exercises the resurrectEntity ladder
        // (canUseAttack -4 BEFORE hasCooldown -3, dead-only targets -6, one
        // crit getDouble on success, ActionResurrect + half-life revival,
        // Order re-insertion before the next initial-order survivor).
        var reviveEffects = Json.createArray();
        var resurrectEffect = Json.createObject();
        resurrectEffect.put("id", 15); // EFFECT_RESURRECT (EffectResurrect is empty)
        resurrectEffect.put("value1", 0);
        resurrectEffect.put("value2", 0);
        resurrectEffect.put("turns", 0);
        resurrectEffect.put("targets", 31);
        resurrectEffect.put("modifiers", 0);
        reviveEffects.add(resurrectEffect);
        Chips.addChip(new Chip(
            REVIVE_CHIP_ID, /*cost*/ 3, /*minRange*/ 1, /*maxRange*/ 8, reviveEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 4, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ REVIVE_CHIP_ID, "revive", ChipType.HEAL,
            /*maxUses*/ -1));

        // 1049 "colossus": TYPE_MULTIPLY_STATS ×2 for 3 turns, self-cast,
        // cooldown 2 — base-stat ×(factor−1) buffs, the first-apply vs
        // replacement max-life delta (cooldown 2 < turns 3 lets a recast
        // replace the live effect) and the ratio-preserving silent heal.
        var colossusEffects = Json.createArray();
        var multiplyEffect = Json.createObject();
        multiplyEffect.put("id", 62); // EFFECT_MULTIPLY_STATS
        multiplyEffect.put("value1", 2);
        multiplyEffect.put("value2", 0);
        multiplyEffect.put("turns", 3);
        multiplyEffect.put("targets", 31);
        multiplyEffect.put("modifiers", 0);
        colossusEffects.add(multiplyEffect);
        Chips.addChip(new Chip(
            COLOSSUS_CHIP_ID, /*cost*/ 2, /*minRange*/ 0, /*maxRange*/ 0, colossusEffects,
            /*launchType*/ (byte) 7, /*area*/ (byte) 1, /*los*/ false,
            /*cooldown*/ 2, /*teamCooldown*/ false, /*initialCooldown*/ 0,
            /*level*/ 1, /*template*/ COLOSSUS_CHIP_ID, "colossus", ChipType.BOOST,
            /*maxUses*/ -1));

        // Bulb template 1001 "harness_bulb" — stat ranges scaled by the
        // OWNER's level (10 → coeff 1/30, bulb_base truncating), ×1.2 on a
        // critical summon. Chips laser (damage) + cure (heal): the bulb
        // fights. Registered after the chips above so Chips.getChip
        // resolves; bulbs added post-init get NO initial cooldowns.
        var bulbChips = Json.createArray();
        bulbChips.add(LASER_CHIP_ID);
        bulbChips.add(CURE_CHIP_ID);
        var bulbStats = Json.createObject();
        bulbStats.putArray("life").add(100).add(400);
        bulbStats.putArray("strength").add(50).add(200);
        bulbStats.putArray("wisdom").add(0).add(100);
        bulbStats.putArray("agility").add(30).add(300);
        bulbStats.putArray("resistance").add(0).add(0);
        bulbStats.putArray("science").add(0).add(100);
        bulbStats.putArray("magic").add(0).add(0);
        bulbStats.putArray("tp").add(4).add(8);
        bulbStats.putArray("mp").add(3).add(6);
        Bulbs.addInvocationTemplate(
            new BulbTemplate(HARNESS_BULB_ID, "harness_bulb", bulbChips, bulbStats));
    }

    static void attach(Fight fight, FarmerLog farmerLog, Leek leek, Class<?> aiClass) {
        leek.setAIFile(new InjectedAIFile("<emitted_" + leek.getId() + ">", leek.getId(), aiClass));
        leek.setLogs(new LeekLog(farmerLog, leek));
        leek.setFight(fight);
        leek.setBirthTurn(1);
        if (Weapons.getWeapon(PISTOL_WEAPON_ID) != null) {
            leek.addWeapon(Weapons.getWeapon(PISTOL_WEAPON_ID));
        }
        if (Chips.getChip(VENOM_CHIP_ID) != null) {
            leek.addChip(Chips.getChip(VENOM_CHIP_ID));
            leek.addChip(Chips.getChip(PROTEIN_CHIP_ID));
            leek.addChip(Chips.getChip(MAGNET_CHIP_ID));
            leek.addChip(Chips.getChip(GLOVE_CHIP_ID));
            leek.addChip(Chips.getChip(PLAGUE_CHIP_ID));
            leek.addChip(Chips.getChip(BLINK_CHIP_ID));
            leek.addChip(Chips.getChip(HOOK_CHIP_ID));
            leek.addChip(Chips.getChip(LASER_CHIP_ID));
            leek.addChip(Chips.getChip(STORM_CHIP_ID));
            leek.addChip(Chips.getChip(BLESSING_CHIP_ID));
            leek.addChip(Chips.getChip(CURE_CHIP_ID));
            leek.addChip(Chips.getChip(REGEN_CHIP_ID));
            leek.addChip(Chips.getChip(WALL_CHIP_ID));
            leek.addChip(Chips.getChip(MIRROR_CHIP_ID));
            leek.addChip(Chips.getChip(ICE_CHIP_ID));
            leek.addChip(Chips.getChip(MUD_CHIP_ID));
            leek.addChip(Chips.getChip(ARMOR_CHIP_ID));
            leek.addChip(Chips.getChip(SWAP_CHIP_ID));
            leek.addChip(Chips.getChip(SPRING_CHIP_ID));
            leek.addChip(Chips.getChip(FORTRESS_CHIP_ID));
            leek.addChip(Chips.getChip(REFLEX_CHIP_ID));
            leek.addChip(Chips.getChip(HASTE_CHIP_ID));
            leek.addChip(Chips.getChip(FOCUS_CHIP_ID));
            leek.addChip(Chips.getChip(SAGE_CHIP_ID));
            leek.addChip(Chips.getChip(BRICK_CHIP_ID));
            leek.addChip(Chips.getChip(WEAKEN_CHIP_ID));
            leek.addChip(Chips.getChip(NUMB_CHIP_ID));
            leek.addChip(Chips.getChip(DULL_CHIP_ID));
            leek.addChip(Chips.getChip(HUSH_CHIP_ID));
            leek.addChip(Chips.getChip(CLEANSE_CHIP_ID));
            leek.addChip(Chips.getChip(UNRAVEL_CHIP_ID));
            leek.addChip(Chips.getChip(JAVELIN_CHIP_ID));
            leek.addChip(Chips.getChip(COMET_CHIP_ID));
            leek.addChip(Chips.getChip(STATUE_CHIP_ID));
            leek.addChip(Chips.getChip(GHOST_CHIP_ID));
            leek.addChip(Chips.getChip(CURSE_CHIP_ID));
            leek.addChip(Chips.getChip(TOXIN_CHIP_ID));
            leek.addChip(Chips.getChip(REAPER_CHIP_ID));
            leek.addChip(Chips.getChip(LEECH_CHIP_ID));
            leek.addChip(Chips.getChip(CATACLYSM_CHIP_ID));
            leek.addChip(Chips.getChip(DOOM_CHIP_ID));
            leek.addChip(Chips.getChip(MUTATION_CHIP_ID));
            leek.addChip(Chips.getChip(CLARITY_CHIP_ID));
            leek.addChip(Chips.getChip(BULWARK_CHIP_ID));
            leek.addChip(Chips.getChip(RUPTURE_CHIP_ID));
            leek.addChip(Chips.getChip(PURGE_CHIP_ID));
            leek.addChip(Chips.getChip(TRANSFUSION_CHIP_ID));
            leek.addChip(Chips.getChip(SPAWN_CHIP_ID));
            // 49th and 50th chips — exactly fills the leek's 50 RAM.
            leek.addChip(Chips.getChip(REVIVE_CHIP_ID));
            leek.addChip(Chips.getChip(COLOSSUS_CHIP_ID));
        }
    }

    public static void main(String[] args) throws Exception {
        if (args.length < 2) {
            System.err.println("usage: Harness <AIClass1> <AIClass2> [seed]");
            System.exit(2);
        }
        Class<?> ai1Class = Class.forName(args[0]);
        Class<?> ai2Class = Class.forName(args[1]);
        int seed = args.length > 2 ? Integer.parseInt(args[2]) : 1;

        Generator generator = new Generator();
        registerPistol();
        registerChips();
        Outcome outcome = new Outcome();

        Fight fight = new Fight(generator);
        final HashMap<Integer, String> registerStore = new HashMap<>();
        fight.getState().setRegisterManager(new RegisterManager() {
            @Override public String getRegisters(int leek) { return registerStore.get(leek); }
            @Override public void saveRegisters(int leek, String registers, boolean is_new) { registerStore.put(leek, registers); }
        });
        LocalTrophyManager statistics = new LocalTrophyManager();
        fight.setStatisticsManager(statistics);
        fight.getState().seed(seed);

        FarmerLog farmerLog = new FarmerLog(fight, 0);
        outcome.logs.put(0, farmerLog);
        fight.getState().setRestatPotionsAvailable(0, 999);

        Leek leek1 = defaultLeek(1, args[0]);
        Leek leek2 = defaultLeek(2, args[1]);
        fight.getState().addEntity(0, leek1);
        fight.getState().addEntity(1, leek2);
        attach(fight, farmerLog, leek1, ai1Class);
        attach(fight, farmerLog, leek2, ai2Class);

        fight.startFight(true);
        fight.finishFight();

        // Mirror Generator.runScenario's Outcome population.
        outcome.fight = fight.getState().getActions();
        outcome.fight.dead = fight.getState().getDeadReport();
        outcome.winner = fight.getWinner();
        outcome.duration = fight.getState().getDuration();
        outcome.statistics = statistics;
        outcome.executionTime = fight.executionTime;

        System.out.println(MARKER);
        System.out.println(outcome.toJson().toString());
    }
}
