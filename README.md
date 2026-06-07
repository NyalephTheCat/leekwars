# Leekscript

A from-scratch **Rust implementation of the LeekScript language and toolchain** —
the scripting language behind [LeekWars](https://leekwars.com), the online game
where you program AIs for "leeks" that fight on a grid.

This workspace is more than a compiler: it ships a `cargo`-style project tool, a
language server, a debugger, multiple code-generation backends, and a native
**fight simulator** so you can write, test, and debug leek AIs entirely offline.

```leek
// @version: 4
function factorial(n) {
    if (n <= 1) {
        return 1;
    }
    return n * factorial(n - 1);
}
factorial(5);
```

```console
$ miku run
120
```

## Quick start

The repo vendors the reference implementations as submodules, so clone
recursively:

```sh
git clone --recursive <repo-url> Leekscript
cd Leekscript
cargo build              # builds the whole workspace (stable Rust)
```

Create and run a project with `miku`, the workspace tool:

```sh
cargo run -p miku -- new hello
cd hello
miku run                 # interpret src/main.leek
miku check               # diagnostics across the project
miku test                # run everything under tests/
```

A project is described by a `Miku.toml` manifest (the `cargo`-equivalent of
`Cargo.toml`): `[project]` metadata, source/test paths, backend settings, lint
and format rules.

## The toolchain

| Binary      | Role                                                          |
|-------------|--------------------------------------------------------------|
| `miku`      | Workspace tool — build/run/test/lint/fmt/doc & more (`cargo`) |
| `leekc`     | Single-file compiler driver (`rustc`)                        |
| `leek-lsp`  | Language server (diagnostics, completion, inlay hints)       |
| `leek-dap`  | Debug Adapter Protocol server (breakpoints, stepping)        |
| `leekbench` | Benchmark runner                                             |

### `miku` at a glance

```text
miku new|init        create / initialize a project
miku build           compile via the manifest's backend (Java by default)
miku run             build with the interpreter and execute
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
miku fight           run / test / debug leek-wars fights  (see below)
```

Global flags include `--manifest-path`, `--library leekwars` (load host
function libraries), `--message-format {human|json|junit}`, and `--color`.

## Fights — the leek-wars simulator

`miku fight` runs a fight described by a **scenario file** (TOML, or the official
generator's JSON) and is built for iterating on AIs:

```sh
miku fight duel.toml                       # one fight, print the winner
miku fight duel.toml --mode matrix         # sweep seeds × opponents × profiles
miku fight duel.toml --mode tournament \   # round-robin / single-elim leaderboard
    --entrant a.leek --entrant b.leek
miku fight duel.toml --mode random \       # fuzz the AI against random point-buy builds
    --runs 50 --capital 800 --random-stats strength,agility,wisdom
miku fight duel.toml --emit ./duel-fight   # generate a standalone native executable
```

Scenarios are **composable**: a file can `extends` a base arena, pull reusable
leek builds from separate files (`leek = "leeks/hero.toml"`), and carry named
override `[profiles]`. You can also **debug an AI inside a running fight** —
breakpoints fire during the turn loop — via `leek-dap` (set `scenario` /
`fightEntity` in your launch config).

A complete, runnable example lives in [`examples/fight/`](examples/fight/) with
its own README.

## Backends

The compiler lowers source → HIR → MIR and targets several backends:

- **Interpreter** — powers `miku run` / `miku test`.
- **Java** — the default `miku build` output (transpiles to `.java`).
- **Native (Cranelift JIT)** — used by `leek-dap` (debugging) and the fight
  simulator (`leek-generator`); also emits relocatable object files.
- **JAR / WASM** — scaffolded; not yet wired into `miku build`.

LeekScript versions 1–4 are supported, with a per-file `// @version:` pragma and
an optional strict mode.

## Benchmarks

Each backend versus the upstream reference (the official Java LeekScript,
`leekscript.jar`) on three compute-heavy programs. Figures are the
**warm-median inner execution time** (JVM start-up and `javac` excluded) from
the bundled `leekbench` harness, 8 runs each:

```sh
leekbench program.leek --runs 8        # interp · native · java · upstream
```

| Backend                       | `fib(28)` (recursion) | loop, 1M iters | array build+sum, 100k |
|-------------------------------|----------------------:|---------------:|----------------------:|
| **upstream-java** (reference) |                5.6 ms |          12 ms |                 18 ms |
| rust-interp                   |                590 ms |         417 ms |                139 ms |
| rust-native (Cranelift JIT)   |                324 ms |          13 ms |                 38 ms |
| rust-java (transpiled)        |                  — ¹  |          13 ms |                 13 ms |
| _+ JVM start-up² (per run)_   |             _≈ 0.26 s_ |       _≈ 0.26 s_ |             _≈ 0.26 s_ |

<sub>AMD Custom APU (Steam Deck), 8 cores · JDK 25 · rustc 1.93 · release
builds. Indicative micro-benchmarks — results vary by workload.</sub>

- **rust-interp** is correct everywhere but is a straightforward MIR walker —
  roughly 10–100× the reference.
- **rust-native** keeps LeekScript's dynamic (boxed) values, so it's level with
  the JVM on tight numeric loops (~1×) yet pays for call- and allocation-heavy
  code (`fib`, arrays). This harness JIT-compiles on every run, so its times
  *include* compilation.
- **rust-java** transpiles to Java and matches or beats upstream where it runs,
  but ¹ user functions with parameters don't yet compile (the `fib` case fails
  `javac`) — codegen is a work in progress.
- ² The per-program rows are **inner** execution time. The two **Java** backends
  also pay a fixed **≈ 0.26 s** JVM process + class-load tax on *every* fresh
  invocation (program-independent; the Rust backends pay ~0). For the upstream
  path, first-use LeekScript compiler warm-up adds **≈ 1.5 s** more, so a *cold
  one-shot* run is **≈ 1.8 s** before the program executes — which is why the
  instantly-starting Rust backends win end-to-end on single runs (e.g. one
  fight AI), even where the JVM's steady-state inner loop is faster.
- Every backend agrees on the computed result wherever it runs.

## Repository layout

```
bins/        leekc, miku, leek-lsp, leek-dap, leekbench
crates/
  core/      spans, diagnostics, manifest, runtime, prelude, environment
  frontend/  lexer, parser, syntax
  middle/    resolver, types, HIR, MIR, complexity
  backends/  interp, java, native (cranelift), backend registry
  db/        pipeline, recipes, driver (the compilation orchestration)
  game/      leek-game-runtime, leek-generator, leek-scenario  (fights)
  tools/     leek-lsp, leek-dap, leek-fmt, leek-lint, leek-ide, …
  testing/   builtin-suite, test-driver, corpus, bench
editors/     vscode (extension + DAP), nvim (LSP config)
examples/    runnable examples (see examples/fight/)
fuzz/        cargo-fuzz targets (separate nightly workspace)
official/, official-generator/   upstream reference impls (git submodules)
```

## Building & developing

Requires stable Rust (the toolchain is pinned in `rust-toolchain.toml`, with
`rustfmt` and `clippy`).

```sh
cargo build                 # whole workspace
cargo test                  # all tests
cargo test -p leek-scenario # a single crate
cargo clippy                # lints (clippy pedantic at warn level)
cargo fmt
```

## Editor support

- **VS Code** — `editors/vscode/` provides syntax highlighting, the language
  server, and debugging (including "Debug AI in fight").
- **Neovim** — `editors/nvim/` provides syntax/filetype detection and
  `leek-lsp` setup (see its README).

## Reference implementations

`official/leek-wars` (the game client) and
`official-generator/leek-wars-generator` (the upstream Java fight generator) are
included as **git submodules** for reference and parity testing. If you didn't
clone with `--recursive`:

```sh
git submodule update --init --recursive
```

## License

Dual-licensed under **MIT OR Apache-2.0**.
