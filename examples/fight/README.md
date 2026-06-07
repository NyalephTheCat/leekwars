# Fight example

A self-contained example for the `miku fight` subsystem: AIs, reusable leek
builds, composable scenario files, and every way to run them — a single fight,
a sweep, a tournament, randomized builds, a standalone executable, and the
in-fight debugger.

Run everything from this directory.

## Layout

```
Miku.toml            project manifest (makes `miku check`/`fmt` work on ais/)
base-arena.toml      a bare arena (map + seed) meant to be inherited
duel.toml            the main scenario: extends base-arena, pulls leek builds
                     from files, has a profile + a [testing] block
skirmish.toml        a second scenario reusing the arena with other builds/AIs
arena.json           the same shape in the official generator's JSON format
ais/
  hero.leek          aggressive: close in and fire until out of TP
  villain.leek       cautious: advance and take one shot
  tank.leek          holds range a bit, then fires
  coward.leek        kites away and takes potshots
leeks/
  hero.toml          reusable build (stats + weapons), no id/cell/team/ai
  tank.toml          tanky build
  glass-cannon.toml  high damage, low life
.vscode/launch.json  debug configs (standalone + debug-inside-a-fight)
```

### How the scenario files compose

- **`extends`** — `duel.toml` starts with `extends = "base-arena.toml"`, so it
  inherits the map and seed and only adds the combatants.
- **`leek` file references** — each entity sets `leek = "leeks/hero.toml"` to
  pull a reusable build, then overrides only the scenario-specific bits
  (`id`, `cell`, `team`, `ai`). Edit `leeks/hero.toml` and every scenario that
  references it changes.
- **`[profiles.<name>]`** — sparse overrides applied with `--profile`.
- **`[testing]`** — default seeds/opponents/etc. for the test modes (CLI flags
  override them).

TOML is canonical; `arena.json` shows the official generator's JSON shape
(2D `entities`, `random_seed`) loads through the same path.

### Writing AIs

A leek starts a fight **unequipped**: the `weapons` in its build are owned
(in its inventory) but not in hand. An AI must `setWeapon(...)` before it can
`useWeapon(...)`. The example AIs equip their primary weapon with
`setWeapon(getWeapons()[0])`, then fire while `useWeapon` returns `> 0` (a hit).


## Running

`miku fight` auto-registers the leek-wars game library, so no `--library`
flag is needed for fights. (For `miku check`/`fmt` on the AIs you do need it:
`miku --library leekwars check`.)

### One fight

```sh
miku fight duel.toml
miku fight duel.toml --seed 5 --profile aggressive
miku fight skirmish.toml --format json
miku fight arena.json            # the JSON form
```

### Matrix sweep (test against many settings)

```sh
# Uses the [testing] block in duel.toml (seeds 1-3 × two opponents):
miku fight duel.toml --mode matrix
# Or ad-hoc:
miku fight duel.toml --mode matrix --seeds 1,2,3,4,5 \
    --vs ais/tank.leek --vs ais/coward.leek
```

### Tournament (leaderboard)

```sh
miku fight duel.toml --mode tournament \
    --entrant ais/hero.leek --entrant ais/tank.leek \
    --entrant ais/villain.leek --entrant ais/coward.leek \
    --bracket round-robin --games 1,2
```

### Randomized point-buy builds

```sh
# Fight 50 random opponent builds (800 capital across str/agi/wis):
miku fight duel.toml --mode random --runs 50 --capital 800 \
    --random-stats strength,agility,wisdom --random-target opponent --seed 7
```

The report flags any build that beat your hero.

### Generate a standalone executable

```sh
miku fight duel.toml --seed 2 --emit ./duel-fight
./duel-fight          # self-contained: no miku, no source files needed
```

(Requires `cargo`; the build links the whole engine, so it takes a while and
the binary is large — but it runs the exact fight on its own.)

### Debug an AI inside a fight (VS Code)

Open this folder in VS Code with the Leekscript extension, set a breakpoint in
`ais/hero.leek`, and pick **"Debug hero inside the duel"** from the Run panel
(`.vscode/launch.json`). Execution stops at your breakpoint *during the fight's
turn loop* — once per turn the hero AI runs — and locals/step/continue work as
usual. The opponent runs full-speed; breakpoints stay scoped to the entity you
are debugging (`fightEntity`).
