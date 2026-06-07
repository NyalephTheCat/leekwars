import java.util.HashMap;

import com.leekwars.generator.Generator;
import com.leekwars.generator.fight.Fight;
import com.leekwars.generator.leek.FarmerLog;
import com.leekwars.generator.leek.Leek;
import com.leekwars.generator.leek.LeekLog;
import com.leekwars.generator.leek.RegisterManager;
import com.leekwars.generator.outcome.Outcome;
import com.leekwars.generator.test.LocalTrophyManager;

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

    /** Stock leek stats, mirroring the generator's FightTestBase.defaultLeek. */
    static Leek defaultLeek(int id, String name) {
        return new Leek(id, name, 0, 10, 500, 6, 7, 100, 100, 10, 50, 10, 0, 0, 8, 30, 0, false, 0, 0, "", 0, "", "", "", 0);
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
            /*maxUses*/ 0, /*forgotten*/ false));
    }

    static void attach(Fight fight, FarmerLog farmerLog, Leek leek, Class<?> aiClass) {
        leek.setAIFile(new InjectedAIFile("<emitted_" + leek.getId() + ">", leek.getId(), aiClass));
        leek.setLogs(new LeekLog(farmerLog, leek));
        leek.setFight(fight);
        leek.setBirthTurn(1);
        if (Weapons.getWeapon(PISTOL_WEAPON_ID) != null) {
            leek.addWeapon(Weapons.getWeapon(PISTOL_WEAPON_ID));
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
