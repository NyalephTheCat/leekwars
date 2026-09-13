# Architecture

This document describes how the Rust LeekScript toolchain is put together: the
crate layers, how a `.leek` source becomes output, and where the major pieces
live. For the language grammar see [`grammar.md`](grammar.md); for user-facing
usage see the top-level [`README.md`](../README.md) and the per-binary docs
under [`bins/*/README.md`](../bins/).

## Workspace at a glance

The repo is a single Cargo workspace (`resolver = "2"`, edition 2024) split
into small crates grouped by responsibility. The [`fuzz/`](../fuzz/) crate is a
standalone nightly cargo-fuzz workspace and is deliberately excluded from the
stable build.

```
bins/        leekc, miku, leek-lsp, leek-dap, leekbench   (executables)
crates/
  core/      spans, diagnostics, manifest, runtime, prelude, environment, builtins
  frontend/  lexer, parser, syntax (the CST)
  middle/    resolver, types, HIR, MIR, charge, complexity
  db/        pipeline, recipes, driver  (compilation orchestration)
  backends/  java, native (Cranelift), aot-runtime, backend registry
  game/      game-runtime, generator, scenario   (the fight simulator)
  tools/     lsp, dap, fmt, lint, migrate, rewrite, ide
  testing/   builtin-suite, test-driver, test-corpus, bench
editors/     vscode (extension + DAP), nvim (LSP config)
examples/    runnable examples (see examples/fight/)
official/, official-generator/   upstream reference impls (git submodules)
```

## The layering rule

Crates are organized into **layers**, and dependencies may only point *down*
the stack. This keeps the dependency graph acyclic and the boundaries honest.
A crate's layer is the directory it lives in (`crates/<layer>/<crate>`,
`bins/<crate>`, or `xtask/`). The order is:

```
core → frontend → middle → db → backends → game → tools · testing → bins · xtask
```

Layers joined by `·` are peers: they share a rank, and neither may depend on
the other. `game` sits above `backends` (the generator runs AIs on the native
backend) and below `tools` (the debug adapter drives fights).

`cargo xtask check-layers` enforces the rule over the graph reported by
`cargo metadata`, and it is part of the CI gate. For each dependency between
two workspace members:

- **Normal and build dependencies** stay inside their layer or point at a
  lower layer.
- **Dev dependencies** may reach at most one rank higher, peers included, so a
  test can use the next layer up.
- **`leek-pipeline` may depend only on `core`** (dev dependencies excepted).
  It is the generic orchestration substrate and must not know about any
  concrete frontend, middle or backend crate.
- **Every member must live in a layer directory.** A crate anywhere else fails
  the check.

Existing violations are listed, each with a justification, in
[`xtask/layer-allowlist.txt`](../xtask/layer-allowlist.txt). Today that
includes every frontend and middle stage depending on `leek-pipeline`,
`leek-runtime` → `leek-hir`, and `leek-recipes` → `leek-fmt`/`leek-lint`. The
list may only shrink: an entry whose edge no longer breaks the rule fails the
check, so remove it in the same change that fixes the edge.

If you reach for an upward dependency, the abstraction you want usually belongs
in a lower layer (or behind a trait that a lower layer defines and a higher one
implements).

## The compilation pipeline

A `.leek` program flows down the layers:

1. **Frontend** (`leek-lexer`, `leek-parser`, `leek-syntax`) turns source text
   into tokens and then a lossless concrete syntax tree (CST, built on
   `rowan`). Losslessness is what lets `leek-fmt` and the LSP work on real
   source ranges.
2. **Middle** lowers and analyzes:
   - `leek-resolver` binds names and scopes.
   - `leek-types` runs type inference / checking (LeekScript keeps dynamic,
     boxed values but the type info drives unboxing in the native backend).
   - `leek-hir` is the high-level IR; `leek-mir` is the lower control-flow IR
     the backends consume.
   - `leek-charge` models LeekWars' per-operation "ops" budget; `leek-complexity`
     derives per-function big-O / cost estimates (`miku analyze`).
3. **db** (`leek-pipeline`, `leek-recipes`, `leek-driver`) is the orchestration
   layer — a query/recipe system that wires the stages together, caches
   artifacts, and is what the binaries call into. `leek-pipeline` is the
   generic engine; `leek-recipes` defines the concrete steps; `leek-driver`
   ties it to a project/manifest.
4. **Backends** consume MIR:
   - `leek-backend-native` is a Cranelift JIT/AOT backend (`miku run`, and
     `leekc --emit` for a standalone executable, linked via `cc`). Scalars
     whose type is known are unboxed; everything else stays a boxed dynamic
     value. `leek-aot-runtime` is the runtime support linked into AOT binaries.
   - `leek-backend-java` transpiles to Java source for the upstream runtime
     classes.
   - `leek-backends` is the registry that selects between them.

`core` underpins all of it: `leek-span` (source positions), `leek-diagnostics`
(error reporting + codes for `miku explain`), `leek-manifest` (the `Miku.toml`
project model), `leek-runtime`/`leek-prelude`/`leek-builtins`/`leek-environment`
(the value model and host/standard library surface).

## The fight simulator

The `game/` crates implement the LeekWars game offline:

- `leek-game-runtime` is the turn-by-turn fight engine, including the
  **generated** weapon and chip catalogs
  (`src/weapons_gen.rs`, `src/chips_gen.rs`). These are extracted from the
  upstream generator's JSON by [`tools/game-item-extract.sh`](../tools/game-item-extract.sh);
  CI runs it with `--check` to guard against drift, so regenerate with
  `--write` when the upstream data changes.
- `leek-generator` mirrors the official generator's fight setup.
- `leek-scenario` parses composable scenario files (TOML or the official JSON)
  that describe arenas, leek builds, seeds, and override profiles — the input
  to `miku fight`. A runnable example lives in
  [`examples/fight/`](../examples/fight/).

You can debug an AI *inside* a running fight: `leek-dap` exposes breakpoints
that fire during the turn loop.

## Binaries

The user-facing surface lives in [`bins/`](../bins/) (each has its own README):

| binary      | role |
|-------------|------|
| `miku`      | the `cargo`-style workspace tool — build/run/check/test/fmt/lint/fight/… |
| `leekc`     | single-file compiler driver (incl. `--emit` for AOT executables) |
| `leek-lsp`  | language server (stdio) backed by `leek-ide` |
| `leek-dap`  | debug adapter (incl. "debug AI in fight") |
| `leekbench` | benchmark harness (the numbers in the README) |

## Tooling & generated code

Several artifacts are **generated and committed**, with CI checking they don't
drift. The scripts live in [`tools/`](../tools/):

- `game-item-extract.sh` — weapon/chip catalogs (see above).
- `builtin-extract.sh` / `game-builtin-extract.sh` — builtin function tables.
- `cargo xtask check-layers` ([`xtask/`](../xtask/)) — the layering rule.
- `check.sh` — the full quality gate (and the contract CI honors).

When you change one of these inputs, regenerate the output in the same commit so
the drift checks stay green.

## Testing strategy

- **Per-crate** unit/integration tests run in the fast gate
  (`cargo test --workspace --exclude leek-test-corpus`).
- **`leek-test-corpus`** validates thousands of `equals(...)` cases against the
  upstream reference. It is slow and submodule-dependent, gated behind
  `tools/check.sh --full`.
- **`leek-bench` / `leekbench`** measure backend performance against upstream.
- **`fuzz/`** holds nightly cargo-fuzz targets (e.g. parser round-trip).

See [`CONTRIBUTING.md`](../CONTRIBUTING.md) for how to run each of these.
