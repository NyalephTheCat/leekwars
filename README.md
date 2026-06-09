# Leekscript

A from-scratch **Rust implementation of the LeekScript language and toolchain** —
the scripting language behind [LeekWars](https://leekwars.com), the online game
where you program AIs for "leeks" that fight on a grid.

It ships a `cargo`-style project tool, a language server, a debugger, several
code-generation backends, and a native **fight simulator** so you can write,
test, and debug leek AIs entirely offline.

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
miku run                 # JIT-compile and run src/main.leek
miku check               # diagnostics across the project
miku test                # run everything under tests/
```

A project is described by a `Miku.toml` manifest (the `cargo`-equivalent of
`Cargo.toml`): `[project]` metadata, source/test paths, backend settings, lint
and format rules.

## `miku` commands

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
miku fight           run / test / debug leek-wars fights  (see below)
```

Global flags include `--manifest-path`, `--library leekwars` (load host
function libraries), `--message-format {human|json|junit}`, `--color`, and
`--verbose` (e.g. `miku build --verbose` prints per-stage pipeline timings).

Other binaries in the workspace: `leekc` (single-file compiler driver),
`leek-lsp` (language server), `leek-dap` (debug adapter), `leekbench`
(benchmark runner).

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
breakpoints fire during the turn loop — via `leek-dap`. A complete, runnable
example lives in [`examples/fight/`](examples/fight/).

## Benchmarks

Every backend versus the upstream reference (the official Java LeekScript,
`leekscript.jar`), from the bundled `leekbench` harness — steady-state
**warm-median per-run** time on four compute-heavy programs, plus **corpus
correctness** (`equals(...)` cases checked against the reference's expected
value). In each column **bold** is the best — fastest run, or most cases correct;
upstream competes on equal footing (it wins `fib`):

| backend | `fib(28)` | loop, 1M | array, 100k | map, 50k | corpus¹ |
|---|--:|--:|--:|--:|--:|
| rust-native · JIT² | 4.9 ms | **8.5 ms** | 14 ms | 48 ms | **9 238 / 9 238** |
| rust-native · AOT exe³ | 7.0 ms | 10 ms | 27 ms | 57 ms | = JIT |
| rust-java⁴ | 4.5 ms | 12 ms | **2.9 ms** | **4.8 ms** | 1 341 / 1 500 |
| upstream-java⁵ | **4.3 ms** | 11 ms | 7.3 ms | 6.7 ms | reference |

<sub>AMD Custom APU (Steam Deck), 8 cores · JDK 25 · rustc 1.93.1 · release.
Indicative — absolute values drift ±20% with thermal state; the relative picture
is stable. Times exclude one-time compile/build and JVM warm-up.
**¹ Corpus**: native runs the **full** 9 238-case corpus (0 errors, ≈ 359 µs/case
median — almost all Cranelift compile, ≈ 1.7 µs execute; full sweep ≈ 146 s). A
full JVM-backend sweep is ~9 h (seconds/case via `javac`+JVM), so the Java rows
are sampled / omitted.
**² JIT**: re-compiles each run — jit-compile (≈ 0.5 ms) + execute (the speed
cell).
**³ AOT exe**: compiles once (≈ 15 ms codegen + ≈ 0.8 s `cc` link) to a standalone
binary, then pure execution; same codegen as JIT, so identical corpus correctness.
**⁴ rust-java**: a transpiler to Java; corpus figure is a representative
**1 500-case** slice — **89% fully correct** (56 wrong values, 103 compile
failures), the rest concentrated in **user-class codegen**.
**⁵** Both Java backends also pay a fixed ≈ 0.26 s JVM process tax per fresh
invocation (≈ 0.3–1.8 s cold before the program runs); the Rust backends pay ~0.</sub>

- **rust-native** keeps LeekScript's dynamic (boxed) values but unboxes scalars
  whose type is known. It **beats the JVM on the scalar loop** and **matches it
  on recursion** (`fib` ≈ upstream, via param-type specialization — proving an
  untyped parameter is monomorphic and compiling it as an unboxed register), while
  trailing on allocation/hashing (arrays, maps) where values stay boxed. Cranelift
  codegen is sub-millisecond, so each warm JIT run is essentially pure execution.
- **rust-native AOT exe** is the same Cranelift code compiled *ahead of time* to a
  standalone binary — no per-run compilation, first run = steady-state. Ideal for
  a single deployed binary (e.g. a fight AI); it trails the JIT on still-boxing
  code (slower TLS in a standalone binary), close where values are unboxed.
- **rust-java** transpiles to Java, in the same ballpark as upstream; the
  instantly-starting Rust backends still win *end-to-end* on single runs since the
  JVM pair pays its start-up tax first.
- The **native** backend matches the reference on the entire corpus
  (9 238 / 9 238); `rust-java`'s remaining mismatches are in the footnote above.

## Repository layout

```
bins/        leekc, miku, leek-lsp, leek-dap, leekbench
crates/
  core/      spans, diagnostics, manifest, runtime, prelude, environment
  frontend/  lexer, parser, syntax
  middle/    resolver, types, HIR, MIR, complexity
  backends/  java, native (cranelift), backend registry
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

Requires stable Rust (pinned in `rust-toolchain.toml`, with `rustfmt` and
`clippy`).

```sh
cargo build                 # whole workspace
cargo test                  # all tests
cargo test -p leek-scenario # a single crate
cargo clippy                # lints (clippy pedantic at warn level)
cargo fmt
```

If you didn't clone with `--recursive`, fetch the reference-implementation
submodules (`official/`, `official-generator/`):

```sh
git submodule update --init --recursive
```

Editor support lives in `editors/`: **VS Code** (`editors/vscode/` — syntax,
language server, "Debug AI in fight") and **Neovim** (`editors/nvim/` — filetype
detection and `leek-lsp` setup).

## License

Dual-licensed under **MIT OR Apache-2.0**.
