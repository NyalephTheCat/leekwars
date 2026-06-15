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

Other binaries in the workspace: [`leekc`](bins/leekc/) (single-file compiler
driver), [`leek-lsp`](bins/leek-lsp/) (language server),
[`leek-dap`](bins/leek-dap/) (debug adapter), and
[`leekbench`](bins/leekbench/) (benchmark runner). Each directory under
`bins/` has a README covering how to build, run, and use that binary.

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

Every backend versus the upstream reference (the official Java LeekScript),
from the bundled `leekbench` harness — steady-state **warm-median per-run**
time on four compute-heavy programs (checked in under
[`crates/testing/leek-bench/fixtures/`](crates/testing/leek-bench/fixtures/)),
plus **corpus correctness** (`equals(...)` cases checked against the
reference's expected value). In each column **bold** is the best — fastest
run, or most cases correct; upstream competes on equal footing (it wins
`array` and `map`, `rust-java` edges `fib`, and `rust-native` takes the
scalar loop and the corpus):

| backend | `fib(28)` | loop, 1M | array, 100k | map, 50k | corpus¹ |
|---|--:|--:|--:|--:|--:|
| rust-native · JIT² | 5.3 ms | **7.7 ms** | 38 ms | 37 ms | **9 519 / 9 519** |
| rust-native · AOT exe³ | 6.9 ms | 9.4 ms | 42 ms | 46 ms | = JIT |
| rust-java⁴ | **4.6 ms** | 12 ms | 6.4 ms | 5.5 ms | 9 320 / 9 519 |
| upstream-java⁵ | 4.6 ms | 12 ms | **3.4 ms** | **3.0 ms** | reference |

<sub>4-core Intel Xeon @ 2.80 GHz (cloud VM) · OpenJDK 25 · rustc 1.94.1 ·
release. Indicative — absolute values move with the machine and the JVM's
warm samples are noisy (p95 up to ~3× the median); the relative picture is
stable. Times exclude one-time compile/build and JVM start-up/warm-up
(the JVM backends loop inside one warmed-up JVM and report per-iteration
`nanoTime` brackets).
**¹ Corpus**: 9 519 enabled `equals(...)` cases extracted from the pinned
upstream test suite. Native compiles and runs every one and **matches the
reference on all 9 519** — no compile errors, and the newer upstream
features the older table flagged as unimplemented (sets ×160, intervals
×440) now run. Full native sweep ≈ 20 s (≈ 420 µs/case median — almost all
Cranelift compile). The rust-java figure is the **full** corpus via the
batch sweep (`--corpus --fast-java`: one `javac` per batch, one JVM; ≈ 85 s,
27 javac rounds): 9 320 correct, 150 wrong values, 49 compile/emit errors.
**² JIT**: re-compiles each run — jit-compile + execute (the speed cell).
Cranelift codegen is sub-millisecond on these programs, so each warm run is
essentially pure execution.
**³ AOT exe**: compiles once (≈ 0.8 s `leekc`, including the `cc` link) to a
standalone binary; cells are whole-process wall time (incl. ~1 ms process
start). Same codegen as JIT, so identical corpus correctness.
**⁴ rust-java**: a transpiler to Java, benchmarked on the upstream runtime
classes.
**⁵** Steady-state only exists after the JVM tax: a cold single-shot run
pays JVM start + in-JVM compile first (~1 s for the `fib` class), where the
Rust backends pay ~0 (JIT cold run ≈ 6 ms in-process, AOT exe ≈ 7 ms
total).</sub>

- **rust-native** keeps LeekScript's dynamic (boxed) values but unboxes scalars
  whose type is known. It **beats both JVMs on the scalar loop** and stays within
  ~15% on recursion (`fib`, via param-type specialization — proving an untyped
  parameter is monomorphic and compiling it as an unboxed register), while
  trailing ~10× on allocation/hashing (arrays, maps) where values stay boxed.
  Cranelift codegen is sub-millisecond on these programs, so each warm JIT run
  is essentially pure execution.
- **rust-native AOT exe** is the same Cranelift code compiled *ahead of time* to a
  standalone binary — no per-run compilation, first run = steady-state. Ideal for
  a single deployed binary (e.g. a fight AI); it trails the JIT on still-boxing
  code (slower TLS in a standalone binary), close where values are unboxed.
- **rust-java** transpiles to Java, in the same ballpark as upstream; the
  instantly-starting Rust backends still win *end-to-end* on single runs since the
  JVM pair pays its start-up tax first.
- The **native** backend matches the reference on **every one of the 9 519
  corpus cases** — including the sets and intervals the older table counted as
  unimplemented — with no compile errors. See the footnote above.

## Desugaring to official LeekScript

The **leekscript** backend emits valid *official* (non-experimental) LeekScript
from code that uses the experimental features (`enum`, `type` aliases,
`interface`/`implements`, generics, function overloading), with those features
desugared away — `enum`s become a class of `static final` fields, overloaded
functions are renamed uniquely (call sites follow), and type-only constructs are
erased. The emitted program runs identically (it is generated from the same
checked HIR the other backends consume).

```sh
leekc src/main.leek --emit leekscript            # human-readable, comments kept
leekc src/main.leek --emit leekscript --compact  # minified, comments dropped
leekc src/main.leek --emit leekscript --optimize # + constant folding / dead-code
miku build --backend leekscript                  # writes build/leekscript/<stem>.leek
```

Pretty output re-formats cleanly and carries the original comments over (recovered
by source span); compact output is minified. `--optimize` runs a small set of
semantics-preserving HIR→HIR passes (constant folding, dead-code elimination)
first. Round-trip and JIT-equivalence tests live in
[`crates/backends/leek-backend-leekscript/tests/`](crates/backends/leek-backend-leekscript/tests/).

## Repository layout

```
bins/        leekc, miku, leek-lsp, leek-dap, leekbench
crates/
  core/      spans, diagnostics, manifest, runtime, prelude, environment
  frontend/  lexer, parser, syntax
  middle/    resolver, types, HIR, MIR, complexity
  backends/  java, native (cranelift), leekscript (source), backend registry
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

Editor support lives in `editors/`: **VS Code**
([`editors/vscode/`](editors/vscode/) — syntax, language server, "Debug AI in
fight"; its README covers building, packaging, and installing the extension)
and **Neovim** ([`editors/nvim/`](editors/nvim/) — filetype detection and
`leek-lsp` setup).

To install the binaries on your `PATH`:

```sh
cargo install --path bins/miku       # and likewise bins/leekc, bins/leek-lsp,
                                     # bins/leek-dap, bins/leekbench
```

`tools/check.sh` is the repo-wide quality gate (fmt + layer check + clippy +
tests; `--full` adds the upstream corpus suite).

## License

Dual-licensed under **MIT OR Apache-2.0**.
