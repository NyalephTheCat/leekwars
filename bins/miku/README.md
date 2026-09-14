# miku

The Leekscript **workspace tool** — the `cargo`-equivalent. It drives whole
projects described by a `Miku.toml` manifest: building, running, testing,
formatting, linting, migrating, documenting, and simulating leek-wars fights.

## Build & install

```sh
# from the repository root
cargo build -p miku                  # debug build → target/debug/miku
cargo build -p miku --release        # release build → target/release/miku

cargo run -p miku -- <args>          # run without installing
cargo install --path bins/miku       # install `miku` into ~/.cargo/bin
```

`cargo install` puts the binary on your `PATH` (assuming `~/.cargo/bin` is on
it), which is the comfortable way to use `miku run` / `miku check` from inside
a Leekscript project directory.

## Quick start

```sh
miku new hello          # scaffold a project (Miku.toml, src/main.leek, tests/)
cd hello
miku run                # JIT-compile and execute src/main.leek
miku check              # diagnostics across the whole project
miku test               # run everything under tests/
```

## Commands

```text
miku new|init        create / initialize a project
miku build           compile via the manifest's backend (Java by default)
miku run             JIT-compile and execute
miku check           diagnostics only
miku test            run tests under tests/
miku fmt | lint      format / lint .leek sources
miku fix             apply machine-applicable diagnostic suggestions
miku explain <CODE>  extended help for a diagnostic code
miku migrate         rewrite sources between language versions (v1–v4)
miku analyze         per-function complexity / big-O
miku profile         run under the ops profiler
miku doc             generate HTML API docs into build/doc/
miku lsp             start the language server on stdio
miku fight           run / test / debug leek-wars fights
miku completions     generate shell completions (bash, zsh, fish, …)
miku clean           remove the build output root (--doc: docs only)
```

Global flags include `--manifest-path` (point at a `Miku.toml` elsewhere;
otherwise `miku` walks up from the current directory), `--library leekwars`
(load host function libraries), `--message-format {human|json|junit}`,
`--color`, `--quiet`, and `--verbose` (e.g. `miku build --verbose` prints
per-stage pipeline timings).

## Output layout

Everything `miku` generates goes under **one output root**: `build/` by
default, overridable per project.

```toml
[paths]
src   = "src"          # sources (default)
tests = "tests"        # tests (default)
build = "build"        # output root (default)
```

```text
build/java/            backend artifacts (java, leekscript, native, …)
build/doc/             miku doc pages (`--out-dir` overrides)
build/fight-reports/   bare `miku fight --report` (see [fight].reports_dir)
```

`miku clean` removes that whole root, generated docs included;
`miku clean --doc` removes only `<build>/doc/`. Since `miku clean` deletes
the root wholesale, `paths.build` must be a relative path inside the project
— `..`, `.` and absolute paths are manifest errors.

## Fights

`miku fight` runs a fight described by a scenario file (TOML, or the official
generator's JSON format):

```sh
miku fight duel.toml                       # one fight, print the winner
miku fight duel.toml --mode matrix         # sweep seeds × opponents × profiles
miku fight duel.toml --mode tournament \
    --entrant a.leek --entrant b.leek      # round-robin / single-elim leaderboard
miku fight duel.toml --mode random \
    --runs 50 --capital 800                # fuzz the AI against random builds
miku fight duel.toml --emit ./duel-fight   # standalone native executable
miku fight duel.toml --report              # also write the JSON report to a file
miku fight duel.toml --report=run.json     # ... at a path of your choosing
```

### The `[fight]` manifest table

```toml
[fight]
default_scenario = "duel.toml"        # what a bare `miku fight` plays
scenarios_dir    = "scenarios"        # where scenario arguments are looked up
reports_dir      = "build/fight-reports"  # where a bare `--report` writes (default)
jobs             = 4                  # sweep workers
```

All four keys are optional. A scenario argument is resolved as given first,
then under `scenarios_dir`, then against the project root. A bare `miku fight`
with no `default_scenario` set lists the `.toml`/`.json` scenarios sitting in
`scenarios_dir`, so you can pick one. A bare `--report`
writes `<mode>.json` (`single.json`, `matrix.json`, …) under `reports_dir`;
`--report=<PATH>` overrides it. `jobs` is parsed, validated (`>= 1`) and
exposed on the manifest, but the matrix / tournament / random drivers still
run sequentially — it takes effect when they learn to run in parallel.

Turn order is drawn from the fight's seed, like the official generator's
`StartOrder` — not from the entity ids — so no side opens by construction. In
a tournament every seed is played twice, the entrants swapping team
slots between the legs, so neither entrant keeps whatever edge a slot carries;
a single-elimination match that ends level is scored as a draw for both
entrants, and which of them advances is a coin drawn from the pairing rather
than the bracket position.

The seeds a pairing plays come from `--seeds` (or `[testing] seeds`). With none
given, `--games N` (or `[testing] games`) derives `N` of them from the
scenario's own seed — the first game keeps that seed, so `--games 1` is the
single game a bare tournament plays. Spelling out both is rejected rather than
silently resolved.

An entrant takes over the **lead (first-listed) entity** of a team; the rest of
the team keeps the AI the scenario gave it. `--entrant-scope team` (or
`entrant_scope = "team"` in the scenario's `[testing]` table) hands the whole
team to the entrant instead.

A tournament has no hero team, so it reports no win/loss totals: each game says
which entrant won it (`"scoring": "leaderboard"` and a per-cell
`winner_entrant` in the JSON, `wins`/`losses`/`draws`/`win_rate` null), and the
leaderboard is the result. The run's exit status is still a gate: non-zero when
a game couldn't be run at all — a matrix or random run also fails when the hero
lost a fight.

Fights work outside a project too: with no `Miku.toml` in scope the defaults
apply, so `--report` writes under `build/fight-reports/`.

A complete, runnable example — AIs, reusable leek builds, composable
scenarios, and debugger launch configs — lives in
[`examples/fight/`](../../examples/fight/).

## The `Miku.toml` schema

Every key below either changes what `miku` does or says out loud that it does
not. An unknown top-level table is an error (`E0401`); an unknown key inside a
known table is a warning (`W0400`); a key that is *in* the schema but that
nothing in this toolchain reads is a warning naming the reason (`W0402`).
Silence is not an option any of them has — a key you can set that quietly does
nothing is worse than a message.

Manifest warnings are ordinary diagnostics, so `[lint]` governs them:
`allow = ["W0402"]` silences the ignored-key notices, `deny = ["W0402"]` turns
them into errors.

```toml
[project]                       # required
name        = "my-leek"         # required
version     = "0.1.0"           # required
language    = 4                 # default `@version` for sources (1..=4)
strict      = false             # default `@strict`
entry       = "src/main.leek"   # entry point
description = "…"               # rendered by `miku doc`
authors     = ["…"]             # rendered by `miku doc`
license     = "MIT"             # rendered by `miku doc`
repository  = "https://…"       # rendered by `miku doc`

[paths]
src     = "src"                 # sources
tests   = "tests"               # tests
build   = "build"               # the single output root `miku clean` removes
benches = "benches"             # W0402: benches are not run (see [bench])

[backend.<kind>]                # kind: java | jar | native | wasm | leekscript
enable  = true
default = true                  # at most one backend, and it must be enabled
out_dir = "build/java"          # artifact directory

[backend.java]
mode       = "exact"            # "exact" | "clean"
emit_lines = false              # write a `.lines` sidecar

[backend.native]
out            = "bin/app"      # the standalone executable `miku build` writes
                                # (`out_dir` and `--out-dir` take precedence)
                                # (`out_dir` and `--out-dir` take precedence)
opt_level      = "speed"        # "none" | "speed" | "speed-and-size"
max_call_depth = 5000           # nested user calls before STACKOVERFLOW
target         = "…"            # W0402: the native backend targets the host

[backend.jar]
main_class = "Main"             # W0402: the jar backend is not implemented

[lint]
deny     = ["L0001"]            # severity overrides, by code or rule name
warn     = []
allow    = ["W0402"]
pedantic = false                # run the pedantic group
nursery  = false                # run the nursery group

[test]
timeout   = 250000              # per-test budget in *operations*
parallel  = false               # W0402: the runner is sequential
junit_xml = "build/tests.xml"   # where --message-format junit writes

[format]                        # see `miku fmt --help`
[fight]                         # see "The `[fight]` manifest table" above
```

Keys outside a backend's own list warn: `mode` and `emit_lines` are java-only,
`out`, `opt_level`, `max_call_depth` and `target` are native-only (`out` and
`main_class` are also legal on `jar`), so `[backend.native] mode = "clean"` is
a `W0400` rather than a setting that quietly does nothing.

`[lsp]`, `[bench]`, `[experimental]`, `[profiles]`, `[profile]`, `[workspace]`
and `[toolchain]` parse and warn as deferred tables (`W0401`).

### Keys that were removed

`project.edition` and `backend.<kind>.java_version` parsed into fields nothing
ever read, and neither has a concept behind it (`project.language` plus the
`@version` pragma is the whole versioning axis; `leek_backend_java::Options`
has no java-version knob). They are out of the schema and now report as
unknown keys.

Two keys changed type, both from a value nothing read to one that is enforced:
`test.timeout` is an integer op budget rather than a duration string (the
runner budgets operations; there is no wall clock), and
`backend.native.opt_level` is one of `leekc --opt-level`'s names rather than an
integer. A manifest still spelling them the old way now gets an error instead
of silence.

## Shell completions

```sh
miku completions zsh > ~/.zfunc/_miku      # or bash/fish/elvish/powershell
```
