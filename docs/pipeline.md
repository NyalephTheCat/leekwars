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

`file_diagnostics_upto(file, stage)` is the single-file counterpart: the same
stage slicing over the per-file cascade, parsing under the empty
`ProgramClasses` set so `include(…)` is left unresolved. `leekc`'s textual
emits (`tokens`, `flat-cst`, `cst`, `fmt`) want exactly that — they describe
the bytes in front of them, and the formatter must stay byte-faithful to the
file it was handed. `leek_session::Scope` is which of the two a compilation
asks for. At `Stage::Tokens` the two answer identically, because resolving an
include means lexing it, which is already past that stage.

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

## What the query layer replaced

The workspace ran two orchestration models side by side until epic
[R1](https://github.com/NyalephTheCat/leekwars/issues/345) collapsed them.
`leek-pipeline` was the older one: a `Step` trait, a `TypeId`-keyed `Context`
artifact bag, and a `RecipePlan` that ordered steps by climbing each
artifact's declared `Requires`. Every pass shipped *both* a step and a tracked
query, and `Step::run` dispatched into the query when `Context::salsa()`
returned `Some` — so the cache existed but only the LSP reached it.

All of that is gone. Each pass crate's `pipeline` module is now its tracked
query and nothing else; `leek-query` (in `core`, because every pass writes its
queries against it) is the database, the two query keys and the timing sink;
`leek-db` is the façade and owns the whole-program queries; `leek-session` is
what a front-end holds.

Three things the deletion settled, worth recording because each was a surprise:

- **A `Run`'s diagnostics were target-dependent.** A run reported what the
  steps it *ran* reported, so its stream grew with the target, while
  `program_diagnostics` always reported the whole frontend. `Stage` and
  `program_diagnostics_upto` are that behaviour, restated as a slice.
- **`leek_parser::pipeline::Parse` was the one step that could abort a run.**
  It was the single production implementor of `RecipeStepStopOnError`, so with
  `stop_on_diagnostics` set, a parse error stopped the pipeline before any
  later step. Tracked passes have no such notion — they work off the green
  tree, which always exists — so `Compilation::query_diagnostics` applies the
  rule on top of them. The LSP never saw it: its params are permissive.
- **An include-aware run parsed a zero-include entry twice**
  ([#522](https://github.com/NyalephTheCat/leekwars/issues/522)). The
  `ResolveIncludes` step published a class set unconditionally, which was
  exactly the gate the `Parse` step used to decide the query could not serve
  it. Routing the whole program through `resolve_program` /
  `typecheck_program` / `lower_program` was the fix, and the defect is gone
  with the step.

Timing did not survive the move as it was. `miku build --verbose`,
`miku dev pipeline` and `leek-bench` printed a duration per pass; there are no
passes, and salsa cannot supply the numbers — it fires `WillExecute` *before* a
query body runs and nothing on completion, so its event stream says which
queries recomputed and never how long any took. (That signal is genuinely
useful, and `leek_db::testing` is built on it; it is simply not this one.) All
three print per-*stage* timings now: the stages a `Compilation` is asked for.

### What is left

- **`leek-session` → `leek-lint`** is one of the two remaining entries in
  [`xtask/layer-allowlist.txt`](../xtask/layer-allowlist.txt), because a
  `Target::Linted` compilation's diagnostics include the lint findings and
  `crates/db` may not depend on `crates/tools`. The linter is a pass over HIR
  rather than a tool, so the fix is to move it into `crates/middle` beside the
  passes it walks — tracked as part of ARCH-03.
- **`cargo xtask graph`**, the mermaid crate-graph with a drift check
  recommended by [#204](https://github.com/NyalephTheCat/leekwars/issues/204),
  is not written.
