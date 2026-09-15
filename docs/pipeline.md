# The query layer

How a `.leek` file becomes answers: the inputs the database holds, the queries
that read them, and what an edit invalidates. For where the crates sit relative
to each other see [`architecture.md`](architecture.md); for the front-end rules
these queries implement (file identity, `include` expansion, version
resolution) see [`semantics.md`](semantics.md).

There is exactly **one** salsa database in the workspace. `leek-db` is the
façade over it: `leek_db::{Db, LeekDb, SourceFile, WorkspaceFiles,
ProgramClasses}` and `leek_db::queries::*` are the whole public surface, so a
consumer needs no dependency on the individual pass crates.

## Inputs

Everything the database knows enters through one of three handles. Change an
input and salsa invalidates exactly the queries that read it.

### `SourceFile` — one file

The unit of identity. A buffer the editor holds open and a file read off disk
are the *same* input when they share a canonical path, so they share every
memo.

| field | why it is an input, not a constant |
|---|---|
| `canonical_path` | The stable identity across revisions, and the key `WorkspaceFiles` maps. Empty for a source with no path at all (an `untitled:` buffer, a string compiled in a test) — hence `String`, not `PathBuf`. |
| `source_id` | `SourceId` as a `u32`; the enum isn't wired through salsa's `Update` yet. |
| `text` | `Arc<str>`, shared rather than owned, so a run takes a refcount bump instead of copying the buffer. |
| `version_byte` | `Version` as its byte, same reason as `source_id`. |
| `strict` | Strict-mode checking. |
| `seed_library` | Whether the checker seeds the typed stdlib signature headers. An input rather than a process global precisely so a query that reads it is invalidated when it flips — the LSP turns it on, the corpus baseline leaves it off. |
| `flags_bits` | Experimental `FeatureFlags` as a bitmask. |

### `WorkspaceFiles` — which files exist

A `BTreeMap<canonical_path, SourceFile>`: the set of files an `include` can
resolve *against*. Only the whole-program queries read it, so adding a file
that nobody includes invalidates nothing downstream of it.

### `ProgramClasses` — the program's class set

Interned, not an input, and passed as a query *argument*. Upstream resolves a
potential type word against the whole program's defined-class set, so
`lowercaseClassFromInclude x = …` is a typed declaration in every file of a
closure that declares it. That set belongs to the *program*, not the file: the
same leaf parses differently under two entries that include it alongside
different siblings.

Keying the parse on it is what makes that cheap. As a field on `SourceFile` it
forced one answer per file, which is why the LSP used to maintain a
workspace-wide union and re-parse every open document whenever anyone typed a
class name ([#163](https://github.com/NyalephTheCat/leekwars/issues/163)). As a
key, two programs' parses of one leaf are two memo entries, and an edit that
leaves the class set alone re-parses only the file that changed.

## Queries

### The per-file cascade

Each stage calls the one above it, so asking for the deepest computes every
stage once:

```
complexity_query ─┐
lower_mir_query ──┤
typecheck_query ──┼─→ lower_hir_query ─→ parse_query ─→ lex_query ─→ pragma_query
resolve_query ────┘                         ▲
                                  ProgramClasses (key)
```

| query | answers |
|---|---|
| `pragma_query(file)` | `// @version:` and experimental opt-ins |
| `lex_query(file)` | tokens + lex diagnostics |
| `parse_query(file, classes)` | the green tree, keyed on the class set |
| `resolve_query(file)` | names and scopes |
| `typecheck_query(file)` | inferred/checked types |
| `lower_hir_query(file)` | HIR (in an `Arc`, so a cache hit is pointer-cheap) |
| `lower_mir_query(file)` | MIR at `O0` — a codegen driver optimizes its own copy |
| `complexity_query(file)` | per-function big-O / cost estimates |

### The include closure

Owned by `leek-db` rather than a pass crate: these span several files and read
`WorkspaceFiles`, which lives here.

| query | answers |
|---|---|
| `include_edges(file)` | one file's include sites, from its `lex_query` |
| `resolve_include(…)` | one include name resolved against the workspace |
| `class_names(file)` | the `class IDENT` heads one file declares |
| `include_graph(files, entry, version)` | the whole closure `entry` reaches |
| `program_classes(files, entry, version)` | the interned union over that closure |
| `include_parse_failures(…)` | files in the closure that would not parse |

### Whole-program passes

`resolve_program`, `typecheck_program` and `lower_program` take
`(files, entry, entry_version)` — `lower_program` an `OptLevel` too — and
answer for the closure, sharing one `parse_query` per file keyed on that
closure's own `program_classes`.

### Whole-program MIR and complexity

`lower_program_mir(files, entry, version, opt)` and
`program_complexity(files, entry, version)` are the include-aware
counterparts of `lower_mir_query` and `complexity_query`, which are keyed on
one file and so answer for the entry alone. The pipeline computes both from
the *merged* program HIR, so the per-file queries would drop every function
an include provides.

### Diagnostics

`diagnostics_without_lints(file)` and `program_diagnostics(files, entry,
version)` compose every query above; `for_source(&stream, source)` takes one
file's slice out of a program-wide stream.

`program_diagnostics_upto(files, entry, version, stage)` is the same stream
stopped after a `Stage` — `Tokens`, `Parsed`, `Resolved`, `TypeChecked` or
`Hir`. Each stage is a prefix of the next. This exists because a `Run`
reports whatever the steps it *ran* reported, so its stream grows with the
target, while `program_diagnostics` always reports the whole frontend;
without slicing, swapping one for the other is a drop-in replacement at
exactly one target and a behaviour change at every other.

Lint findings are deliberately *not* in there. `leek_lint::lint_query` and
`diagnostics_with_lints` live in `leek-lint`, which depends *down* on
`leek-db`; `crates/db` may not depend on `crates/tools`, so a tool's query
belongs in the tool. `leek-fmt`'s `format_query` stays in `leek-fmt` for the
same reason.

## Invalidation

The useful consequences of the shape above:

- **Editing one file re-parses one file.** The include graph is tracked over
  each file's `lex_query`, so an edit that doesn't touch an `include` site
  leaves every *other* file's parse memo valid.
- **Editing a leaf's body doesn't re-parse its siblings.** They share the
  closure's `program_classes`, and a body edit doesn't change it.
- **Declaring or renaming a class does** invalidate the closure's parses —
  that is the one edit that genuinely changes how every file in it parses.
- **A file nobody includes is inert.** Only whole-program queries read
  `WorkspaceFiles`.
- **Flipping `strict` or `seed_library` invalidates from the checker down**,
  and no further up than that, because they are input fields rather than
  process globals.
- **One memo table per query.** `leek_db::queries::parse_query` and
  `leek_parser::pipeline::parse_query` are the same function — the façade
  re-exports, never wraps. A wrapper would be a second memo table over the same
  work.

## The legacy path, and what is left

Two orchestration models still coexist, which is what epic
[R1](https://github.com/NyalephTheCat/leekwars/issues/345) exists to collapse.

`leek-pipeline` is the older one: a `Step` over a `TypeId`-keyed artifact bag,
with stage ordering in `RecipePlan`. Each pass ships *both* a `Step` and a
tracked query, and `Step::run` dispatches to the query when
`Context::salsa()` returns `Some`. `leek-session` (`Session`, `Compilation`)
is what the binaries call; it drives the pipeline and hands out a database
handle.

**Landed.** `leek-db` owns the façade. The LSP is entirely off the pipeline —
no handler plans one; diagnostics come from `program_diagnostics_with_lints`
and formatting from an options-keyed `format_query`. `leek-driver` and
`leek-recipes` are merged into `leek-session`, which now owns one database per
invocation rather than one per compiled file. The include closure is
incremental. salsa is an ordinary workspace dependency.

### Reproducing a `Run`'s answers

Before `Compilation` can move, every answer it serves has to be obtainable
from queries *identically*. That took longer than it looks, because a `Run`
carries three behaviours the query layer has no notion of. Each was found by
a test, and each was invisible until the one before it was fixed:

1. **The target.** A run reports what the steps it ran reported. Handled by
   `Stage` and `program_diagnostics_upto`.
2. **The abort.** `leek_parser::pipeline::Parse` is the single production
   step implementing `RecipeStepStopOnError`, so when the params'
   `stop_on_diagnostics` is set and the parse itself reports at or above it,
   `Context::abort` stops `Pipeline::drive` before any later step. The check
   is on the entry's own parse — the closure's parses happen in
   `ResolveIncludes`, which runs *before* `Parse` and so is not what the
   wrapper counts as new. This is also why the LSP never saw any of it:
   `lsp_params` is `RecipeParams::permissive`, so its runs never abort.
3. **The stage before parsing.** A `Target::Tokens` run never plans `Parse` at
   all, so the abort rule must not fire below `Stage::Parsed`.

`leek_session::Compilation::query_diagnostics` implements all three, and
`query_diagnostics_reproduces_the_run_across_the_matrix` checks it against
real runs over six targets × both threshold settings × a file that parses and
one that does not.

### What is left

Tracked on [#99](https://github.com/NyalephTheCat/leekwars/issues/99):

- **Move `Compilation`.** All-or-nothing, not consumer by consumer: a `Run`
  planned for a late target has already computed everything on the way there,
  so serving one accessor from a query while another reads the run pays for
  both. The queries behind every accessor now exist and are pinned against the
  pipeline, so what remains is the switch itself plus `sources()` and
  `input()`, which still read the run.
- **Then `leek-test-driver` and `leekc`.** `leekc` has no session at all — it
  plans its own pipeline per `--emit` — so it is the last `Run` consumer and
  the one that decides when `Compilation::get::<A>()` can go.
- **Then the deletions.** `Step`, `Context`, `RecipePlan`, `define_step!` and
  the twelve per-crate `pipeline` modules, and
  [`xtask/layer-allowlist.txt`](../xtask/layer-allowlist.txt) shrinks to zero:
  every entry in it today is an edge this migration removes.
