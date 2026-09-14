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
editors/     vscode (extension + DAP), nvim (filetype + syntax)
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
includes every frontend and middle stage depending on `leek-pipeline` and
`leek-session` → `leek-fmt`/`leek-lint`. The
list may only shrink: an entry whose edge no longer breaks the rule fails the
check, so remove it in the same change that fixes the edge.

If you reach for an upward dependency, the abstraction you want usually belongs
in a lower layer (or behind a trait that a lower layer defines and a higher one
implements).

## Error handling

Libraries return **typed errors**; only the outermost layer flattens them into
prose. Concretely, four rules:

**1. A library crate returns a hand-written enum**, with a manual
`impl Display` and `impl std::error::Error`. Not `thiserror`: these enums run
four to eight variants, the workspace already hand-writes them
(`leek_resolver::folder::LoadError`, `leek_backend_native::NativeError`), and
the external dependency list is deliberately small. The enum's variants carry
the *facts* — the offending key, the path, the line — not a pre-rendered
sentence, so a caller can match on what went wrong instead of grepping a
string.

**2. An error with a source location implements
[`leek_diagnostics::IntoDiagnostic`]** and carries a `leek_span::Span` plus a
catalog `Code`. The span is the point of the exercise; the enum is only its
carrier. `leek_manifest::ManifestError` and `leek_resolver::IncludeError` are
the reference shapes. An error that genuinely has no source — a `.lib` catalog
line, a filesystem failure — deliberately does *not* implement the trait, and
says so in a doc comment so the omission reads as a decision.

**3. `anyhow` is for `bins/`, `xtask/` and `crates/testing/` only.** It must
not appear in the public signature of anything under `crates/{core, frontend,
middle, db, backends, game, tools}`: a library that returns `anyhow::Error`
has thrown away the distinction its caller needs. `cargo xtask check-errors`
enforces this over the `cargo metadata` graph, with the remaining violations
listed in [`xtask/error-allowlist.txt`](../xtask/error-allowlist.txt). Like
the layer allowlist it may only shrink.

**4. `Result<_, String>` is banned**, except where a third-party API mandates
it. There is exactly one sanctioned exception today: clap's `value_parser`
signature (`bins/leekc/src/cli.rs`, `parse_version`), which clap defines as
`fn(&str) -> Result<T, String>`.

A stringly-typed error is not "typed in name only" progress either: a struct
with a single `message: String` field is the same error, wearing a hat.

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
3. **db** (`leek-pipeline`, `leek-db`, `leek-session`) is the orchestration
   layer — a query/recipe system that wires the stages together, caches
   artifacts, and is what the binaries call into. `leek-pipeline` is the
   generic engine; `leek-db` is the query façade, re-exporting the one salsa
   database and every tracked query under a single import path so a consumer
   needs neither the pass crates nor their `salsa` features; `leek-session`
   defines the concrete steps (its `recipes` module) and ties them to a
   project/manifest (its `driver` module).
4. **Backends** consume MIR:
   - `leek-backend-native` is a Cranelift JIT/AOT backend (`miku run`, and
     `leekc --emit` for a standalone executable, linked via `cc`). Scalars
     whose type is known are unboxed; everything else stays a boxed dynamic
     value. `leek-aot-runtime` is the runtime support linked into AOT binaries.
   - `leek-backend-java` transpiles to Java source for the upstream runtime
     classes.
   - `leek-backends` is the registry that selects between them.

`core` underpins all of it: `leek-span` (source positions, and `leek_span::paths`
— the one rule for collapsing two spellings of a path to one map key),
`leek-diagnostics` (error reporting + codes for `miku explain`), `leek-manifest`
(the `Miku.toml` project model), `leek-runtime`/`leek-prelude`/`leek-builtins`/`leek-environment`
(the value model and host/standard library surface).

## The fight simulator

The `game/` crates implement the LeekWars game offline:

- `leek-game-runtime` is the turn-by-turn fight engine, including the
  **generated** weapon, chip and bulb catalogs
  (`src/weapons_gen.rs`, `src/chips_gen.rs`, `src/official_items_gen.rs`).
  These are extracted from the
  upstream generator's JSON by [`tools/game-item-extract.sh`](../tools/game-item-extract.sh);
  CI runs it with `--check` to guard against drift, so regenerate with
  `--write` when the upstream data changes.
- `leek-generator` mirrors the official generator's fight setup, down to the
  order of play: it is drawn from the fight's seed before turn 1
  (`StartOrder.compute`), not taken from the entity ids.
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

- `game-item-extract.sh` — weapon/chip/bulb catalogs (see above).
- `builtin-extract.sh` / `game-builtin-extract.sh` — builtin function tables.
- `cargo xtask check-layers` ([`xtask/`](../xtask/)) — the layering rule.
- `cargo xtask check-errors` ([`xtask/`](../xtask/)) — the error convention:
  no `anyhow` in a library layer.
- `cargo xtask check-toolchain` ([`xtask/`](../xtask/)) — the Rust pin is an
  exact release and matches the advertised MSRV.
- `cargo xtask check-artifacts` ([`xtask/`](../xtask/)) — generated output
  stays untracked and git-ignored. The upstream LeekScript compiler writes
  `AI_<id>.java/.class/.lines/.sig` into `./ai` relative to its working
  directory, so the java-emitter scripts run the JVM from a scratch dir under
  `tools/java-emitter/build/` and this check catches anything that slips.
- `check.sh` — the full quality gate (and the contract CI honors).

When you change one of these inputs, regenerate the output in the same commit so
the drift checks stay green.

## Testing strategy

- **Per-crate** unit/integration tests run in the fast gate
  (`cargo test --workspace --exclude leek-test-corpus`).
- **`leek-test-corpus`** validates thousands of `equals(...)` cases against the
  upstream reference. It is slow and submodule-dependent, gated behind
  `tools/check.sh --full` locally and by the `corpus` workflow (nightly + PRs
  touching the compiler) in CI. Of its three columns only `native` checks
  values; `pipeline` is a compile gate and `java-emit` is emit-only.
- **`leek-bench` / `leekbench`** measure backend performance against upstream.
- **`fuzz/`** holds nightly cargo-fuzz targets (e.g. parser round-trip).

See [`CONTRIBUTING.md`](../CONTRIBUTING.md) for how to run each of these.
