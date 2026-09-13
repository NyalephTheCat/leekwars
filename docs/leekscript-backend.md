# LeekScript backend

`crates/backends/leek-backend-leekscript` reads HIR and produces **official**
LeekScript source — the dialect the leek-wars editor accepts, with none of this
toolchain's experimental extensions. Its job is the round trip: anything this
crate emits must re-parse and re-lower with every feature flag off and run to
the same value. `tests/equivalence.rs` is that contract, executed.

Entry point: `emit(&HirFile, &Options) -> EmittedLeekScript`.

## 1. The output is one self-contained file

When an include graph ran — which is every project path: `miku build
--backend leekscript`, `leekc`, anything built on
`leek_recipes::pipeline_hir_with_includes` — the HIR handed to this crate has
already been *bundled*. `leek_hir::lower::lower_files` merges every included
file's definitions into one `HirFile` and splices each `include("name")` site
into the included file's main-block statements, so the emitted `.leek` defines
everything it calls and can be pasted into the editor as-is. There is no
`Options::bundle`: bundling is not a mode, it is the only behaviour.

Three rules follow, and each has a test:

- **No `include(…)` survives.** The splice descends into nested statement
  lists (blocks, switch arms) and into definition bodies, because `include` is
  an ordinary statement and parses anywhere a statement does. A body that holds
  a single boxed statement — `if (c) include("x");`, an unbraced loop body —
  has no list to splice into, so the include expands to a block: an included
  main block is usually more than one statement, and leaving it unbraced would
  put only the first under the branch. Nothing leaks out of that block that did
  not already stay in, because every unit's main block is lowered in its own
  scope.
- **Included definitions are emitted.** Origin is *recorded*, not inferred:
  `Options::prelude_sources` lists the ids that hold merged library headers
  (default: `leek_prelude::source_id()`), and `drop_prelude_defs` drops only
  those. An included file carries its own `SourceId` and its definitions belong
  in the output like the entry's own — including in the overload-rename pass,
  which shares the same predicate.
- **Each global is declared exactly once.** HIR keeps a `Def::Global` item
  *and* every `global x = …;` declaration site, all folded onto one `DefId`.
  `declared_global_defs` settles the item-versus-statement case;
  `Emitter::declared_globals` keeps the `global` keyword on the first statement
  site and degrades the rest to plain assignments (`global g = 1; global g = 2;`
  → `global g = 1; g = 2;`). Order and values are preserved — upstream hoists
  the declaration regardless of position. A repeat site with no initializer
  emits nothing at all.

The lone `Stmt::Include` arm in the emitter is for the *single-file* path,
where no include graph ran: nothing was merged, so re-emitting the include
statement is the faithful output.

## 2. `Options::version`

`Options::version` reaches two places: comment recovery (it lexes the original
source text) and string literals. v1 keeps the backslash before a quote that
matches the delimiter — `"\""` reads back as the two characters `\"` — so at v1
a `"` inside the value cannot be written with a backslash and the literal
switches to `'` delimiters instead.

Nothing else branches on it, which is correct only while the output version
equals the source version. Two known limitations:

- A v1 value containing **both** quote characters has no single-literal form.
  It keeps the v2+ shape, the closest approximation available.
- `lower_files` lowers each included unit at its **own** `@version` pragma,
  while the bundle is one file at one version. `^=` is genuinely
  version-dispatched (POWER-assign at v1, XOR-assign at v2+), so a v1
  `lib.leek` containing `x ^= 5` spliced into a v4 entry emits `x ^= 5` into a
  v4 bundle, where it means XOR. Fixing this needs per-node origin-version
  tracking the HIR does not carry; the short-term guard would be a diagnostic
  when an included unit's version differs from the entry's.

## 3. Testing

`tests/equivalence.rs` runs every case through lower → emit (pretty / compact /
optimized) → re-lower with all features off → JIT, asserting no error
diagnostics and an identical result. `check_at(src, version)` runs that at an
explicit language version; `lower_project(entry, files, flags)` runs a whole
multi-file fixture through the real `ResolveIncludes` step and recipe, so the
include graph and the splicer are exercised rather than simulated.

Assertions about globals must read the emitted **text**
(`count_global_decls`), never only the round-trip result: re-lowering a
duplicated declaration is idempotent in our parser, so a round-trip-only test
passes against the bug.
