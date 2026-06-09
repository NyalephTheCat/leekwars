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
miku run                 # JIT-compile and run src/main.leek
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
miku run             build and execute via the native JIT
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

- **Java** — the default `miku build` output (transpiles to `.java`).
- **Native (Cranelift JIT)** — powers `miku run` / `miku test`, `leek-dap`
  (debugging), and the fight simulator (`leek-generator`); also emits
  relocatable object files. Handles the full middle-end it's given, including
  lambdas/closures, first-class functions, and classes (instances, fields,
  dynamic method dispatch).
- **Native (AOT)** — compiles ahead-of-time to a **standalone executable**
  (`miku build --backend native`, or `leekc --emit native --native-emit exe`).
  Unlike the JIT path it compiles once and the binary runs with no per-run
  codegen. It links the emitted object against a prebuilt static runtime
  archive with `cc` — no per-build `cargo`, so a compile is a sub-second C link.
  It covers the scalar / string / numeric-array / direct-call subset; programs
  using lambdas, classes, or builtin values (`PI`, `var f = abs`) are still
  rejected with a pointer to the JIT. (Those bake compiler-process heap pointers
  as absolute immediates — string and null literals were made relocatable, as
  in-binary bytes built at runtime; extending that to boxed-constant and class
  handles is the remaining AOT work.)
- **JAR / WASM** — scaffolded; not yet wired into `miku build`.

LeekScript versions 1–4 are supported, with a per-file `// @version:` pragma and
an optional strict mode.

### Optimization

Codegen drivers run backend-agnostic passes that shrink a program's **operation
budget** (the per-turn `TOO_MUCH_OPERATIONS` limit in leek-wars):

- **Function inlining** — a call to a small `return <expr>` free function with
  trivial (literal / variable) arguments is replaced by its body, then the call
  disappears (and the result often folds: `dbl(2 + 3)` → `dbl(5)` → `10`).
- **Constant propagation** — an immutable `var x = <literal>` or file-level
  `global G = <literal>` (never reassigned, captured, or passed by reference) is
  substituted at its uses, then its declaration is dropped.
- **Constant folding** — constant expressions collapse to literals (`1 + 2 * 3`
  → `7`, `true ? a : b` → `a`, `"a" + "b"` → `"ab"`), including calls to pure
  math builtins (`abs`, `min`, `max`, `floor`, `ceil`, `sqrt`). Conservative and
  version-independent: division, `%`, `**`, `??`, `round`, casts, and mixed-type
  `==` are left alone so results never diverge across backends.
- **Dead-code elimination** — a constant-condition `if`/`while` collapses to the
  taken branch (or is dropped), and pure discarded expression-statements are
  removed. (Analysis paths like `check`/`lint` stay at `O0`, so they still see
  and report the original code.)
- **Control-flow simplification** — a branch/switch on a constant, or a branch
  whose arms coincide, becomes an unconditional jump, and blocks that become
  unreachable are removed.

These are gated by an optimization level: `miku run` (native JIT) and
`miku build --clean` (readable Java) compile at **O1**; Java *exact* mode (which
mirrors the upstream reference compiler's emission) and the analysis paths
(`check`, `lint`, the LSP) stay at **O0**. `miku build --verbose` prints
per-stage pipeline timings.

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
failures), the rest concentrated in **user-class codegen**. After recent codegen
fixes (range/slice access, `++`/`--` on indexed l-values, builtin-class
constructors, typed-array coercion) + harness corrections (JVM locale,
`ai.export`, log capture); the array subset alone went **220/400 → 396/400**.
**⁵** Both Java backends also pay a fixed ≈ 0.26 s JVM process tax per fresh
invocation (≈ 0.3–1.8 s cold before the program runs); the Rust backends pay ~0.</sub>

- **rust-native** keeps LeekScript's dynamic (boxed) values but unboxes scalars
  whose type is known. It **beats the JVM on the scalar loop** and now **matches
  it on recursion** (`fib` ≈ upstream) thanks to param specialization (next
  bullet); it still trails on allocation/hashing (arrays, maps), where values
  stay boxed. Two wins got it there: a per-run **bump arena** for value boxes
  (replacing a `malloc` per value — allocation-heavy code 2–3× faster) and the
  specialization below. Cranelift codegen is **sub-millisecond** (the JIT's
  per-run compile step), so each warm JIT run is essentially pure execution.
- **Param type specialization.** `fib(n)`'s `n` is untyped, so every `n-1` /
  `n < 2` would box a dynamic value. The native backend proves — by a
  whole-program fixpoint over call sites — that such a parameter only ever
  receives one scalar kind (every site an `integer`, or every site a `real`),
  then compiles it as an unboxed register value (`isub`/`icmp` / float ops, no
  allocation). That alone took `fib` from 117 ms to ≈ 4.5 ms. It applies to any
  provably-monomorphic untyped parameter of a free function or a standalone
  class's public method (including const-default args), not just `fib`.
- **rust-native AOT exe** is the same Cranelift code compiled *ahead of time* to
  a standalone binary (`leekc --emit native --native-emit exe` / `miku build
  --backend native`): no per-run compilation, ~3 ms process start, first run =
  steady-state. It's **thread-local-storage-bound** (a standalone binary's slower
  TLS model makes the per-run op counter / value arena / globals costlier to
  reach), so it trails the JIT most on still-boxing code (arrays, maps) and stays
  close where values are unboxed (scalar loop, specialized `fib` ≈ 6 ms). It pays
  the bigger one-time build (≈ 0.8 s, once) for instant, recompile-free runs —
  ideal for a single deployed binary (e.g. a fight AI), less so for throughput.
- **rust-java** transpiles to Java and runs in the same ballpark as the upstream
  reference — matching it on `fib`, ~20% behind on the loop, and ahead on
  array/map build here. The instantly-starting Rust backends still win *end-to-end*
  on single runs, since the JVM pair pays the ≈ 0.26 s (and, cold, up to ≈ 1.8 s)
  start-up tax before the program even executes.
- The **native** backend matches the reference on the entire corpus (9 238 / 9 238);
  `rust-java`'s remaining mismatches are summarised in the corpus footnote above.

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
