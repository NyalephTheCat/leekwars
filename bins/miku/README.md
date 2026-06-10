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
miku doc             generate HTML API docs
miku lsp             start the language server on stdio
miku fight           run / test / debug leek-wars fights
miku completions     generate shell completions (bash, zsh, fish, …)
miku clean           remove build artifacts
```

Global flags include `--manifest-path` (point at a `Miku.toml` elsewhere;
otherwise `miku` walks up from the current directory), `--library leekwars`
(load host function libraries), `--message-format {human|json|junit}`,
`--color`, `--quiet`, and `--verbose` (e.g. `miku build --verbose` prints
per-stage pipeline timings).

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
```

A complete, runnable example — AIs, reusable leek builds, composable
scenarios, and debugger launch configs — lives in
[`examples/fight/`](../../examples/fight/).

## Shell completions

```sh
miku completions zsh > ~/.zfunc/_miku      # or bash/fish/elvish/powershell
```
