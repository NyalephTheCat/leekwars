# Front-end semantics

This document is the reference for the language rules the front end implements
between "we have a parse tree" and "we have typed HIR": how `include` composes
several files into one program, which `@version` each file is compiled at, how a
bare name resolves to a definition, and what the type lattice admits.

It describes the code as it stands, and every rule below names the function that
implements it. For the crate map and the pipeline as a whole see
[`architecture.md`](architecture.md); for the surface syntax see
[`grammar.md`](grammar.md).

## 1. File identity

Every map keyed by a file — the include graph, the project index, the LSP's
open-buffer table — must collapse two spellings of the same file to one key.
That rule has a single implementation, `leek_span::paths`:

- `canonical_or_normalized(path)` is the key function. It tries
  `std::fs::canonicalize` first and falls back to `normalize_lexical` only for a
  path that does not exist yet (an unsaved buffer, an include target still being
  typed).
- The order is load-bearing. Canonicalizing first is the only form that is
  correct through a symlink; normalizing `real/link/..` lexically yields `real`,
  which is a *different file* from wherever `link` points.
- The results are map keys, not paths to open. Open the path the caller handed
  you; key the map by the canonical form.

`Folder::load` returns the canonical path alongside the file's bytes
(`LoadedFile`) so callers never re-open or re-stat a file to learn its identity.

## 2. Includes

`include("name")` is **inline expansion**, matching upstream's textual splicing.
It is not a module system: there is no namespace, no export list, and no
import-time evaluation order distinct from the splice point.

### 2.1 Resolution

`Folder::resolve(base, name)` interprets `name` relative to the file whose path
is `base` (the *includer*, not the entry). A plain name resolves sibling-to the
includer; a subfolder name (`include("lib/util")`) traverses down. The `Folder`
trait is what lets the compiler be embedded: `DiskFolder` for the CLIs, an
in-memory folder for the LSP, the asset bundle for LeekWars, fakes for tests.

Failures surface as diagnostics, not as `Result` values reaching the user:

| Condition | Diagnostic |
|-----------|------------|
| `name` resolves to no file | `INCLUDE_NOT_FOUND` |
| resolved but unreadable (permissions, not UTF-8) | `INCLUDE_UNREADABLE` |
| the graph contains a cycle | `CIRCULAR_INCLUDE` |

`AI_NOT_EXISTING` is a *different* code, reserved for `import`. Do not conflate
the two — `miku explain` distinguishes them.

### 2.2 Building the graph

`leek_resolver::include_graph::build_include_graph` walks `include(...)` sites
transitively and returns the reachable files topologically ordered, **entry
last**, so a consumer can pre-declare leaves first and let the entry inherit
everything.

Include extraction is version-aware: each file's `include(...)` calls are
scanned after re-lexing that file at *its own* version. Otherwise a v2 file that
uses `and` as a keyword gets tokenized at v4, where `and` is an identifier, and
the cached token stream is wrong for the real compile pass that follows.

### 2.3 Expansion

`leek_hir::lower::lower_files` performs the splice. Two passes:

1. **Pre-declaration.** Top-level declarations from *every* file — functions,
   classes, enums, and `global`s — are registered before any body is lowered, in
   include-graph order. So a declaration is visible everywhere, including above
   its own textual position and across file boundaries. Globals in particular
   are pre-declared so a use above the `global` statement still resolves to
   `NameRef::Global` rather than falling through to a builtin name.
2. **Execution-order splicing.** Main-block statements are lowered in the order
   they run: the entry's children in source order, with each `include("name")`
   site replaced, in place, by the included file's main-block statements —
   lowered *in the scope that is live at the site*. An included file therefore
   sees the locals its includer declared above the site, and only those.

### 2.4 One expander, three passes

Lowering is not the only pass with an execution order: the resolver records
references and redeclarations in the order statements run, and the type checker
records a top-level variable's type when it reaches the declaration. All three
must therefore enter an included file *at its include site*, in the state live
there. They share one implementation,
`leek_resolver::include_graph::IncludeExpander`, so they cannot drift:

- **A file expands at most once**, at the first site that reaches it (the
  `already` set). A diamond include does not duplicate the body.
- **The entry counts as already expanded** from the start. A cycle's back edge
  lands in the `resolved` map before `build_include_graph` detects the cycle, so
  a walker without that seed would recurse forever. (The cycle itself is
  *reported* by the graph walk, not here.)
- **A name that resolves to nothing expands to nothing** — the missing file was
  already reported as `INCLUDE_NOT_FOUND` when the graph was built.

The stack the expander keeps is what makes an `include(...)` inside a function
or class body resolve relative to the file it is *written* in rather than
whatever the main walk last entered; a pass re-roots it with `set_current`
before walking a file's own definitions.

MIR and every backend see a single `HirFile` and never learn that includes
existed. (The LeekScript source backend re-serializes that one file; see
[`leekscript-backend.md`](leekscript-backend.md).)

When there is no include graph at all, `include(...)` is preserved as
`Stmt::Include` instead of being spliced.

## 3. Version resolution

A program is compiled at one of the language versions in `leek_syntax::Version`,
and the version changes lexing (keyword sets), parsing, and a handful of
lowering decisions — v1, for instance, does not process the `\"` escape inside
`"…"` strings.

- **The entry file** uses the version the caller settled — `Input::version_byte`.
  It is never re-derived from the entry's pragma inside the pipeline; the driver
  reads the pragma once, at the boundary.
- **An included file** uses its own explicit `@version` pragma when it has one,
  and otherwise **inherits the entry's version**. A pragma-less include in a v2
  program is lexed, parsed, resolved, checked and lowered at v2, not silently at
  the v4 default.
- Each unit is lowered at its own version; `Lowerer::version` is set per unit and
  never re-derived from pragmas during lowering.

## 4. Name resolution: scopes and boundaries

`Lowerer` carries a stack of lexical scopes (`scopes`) and a parallel stack of
booleans (`boundaries`) marking which of them are *opaque*. Lookup
(`Lowerer::lookup_local`) walks the scope stack innermost-first and stops at the
first opaque scope it crosses.

| Construct | Opens | Opaque? |
|-----------|-------|---------|
| block `{ … }` | `push_scope` | no |
| `for` / `foreach` header + body | `push_scope` | no |
| lambda | `push_scope` | **no** |
| top-level function body | `push_function_scope` | **yes** |
| class method body | `push_function_scope` | **yes** |
| class constructor body | `push_function_scope` | **yes** |

The one surprising row is the lambda. A lambda deliberately opens a
*transparent* scope so that `lookup_local` walks straight through it into the
enclosing function's locals — that is what makes a name a **capture** rather
than an unresolved global, and MIR's `collect_lambda_captures` depends on it. A
lambda that opened an opaque scope would silently stop capturing.

Functions, methods and constructors are opaque for the opposite reason. Inside
`class A { a; m() { … a … } }`, the bare `a` must reach the class-field rewrite
(`this.a`) driven by `class_ctx`, and must *not* first hit an enclosing `var a`
from the main block. Cutting the lookup at the function boundary is what keeps
those two from fighting over the same name.

Globals never enter `scopes` at all. `declare_global` records the name in
`file_decls` (the file-wide declaration map, keyed name → `NameKind`), which is
also what the MIR lowerer reads to build the program-wide `globals` table. Only
`declare_local` writes into a scope. A name that misses both resolves as a
builtin.

## 5. The type lattice

`leek_types::Type` is the checker's type language. It is a *checking* artifact:
MIR and the backends ignore it except where a lowering decision depends on it.

### 5.1 The types

`Any` is the top type and `Null` is universally compatible, because the runtime
is dynamic and a false positive is worse than a miss. Beyond those:

- Primitives: `Boolean`, `Integer`, `Real`, `BigInteger`, `String`, `Interval`,
  `Void` (distinct from `Null` — a `void` function may not `return null`).
- Containers: `Array(T)`, `Map(K, V)`, `Set(T)`, and `Object` for a `{f: v}`
  literal, which is *not* a class instance.
- `ClassInstance(name, args)`. The `args` vector carries bound generic arguments
  for an experimental generic class; it is empty otherwise, and codegen ignores
  it entirely.
- Functions: `Function` (signature unknown) and `FunctionWithReturn { params,
  ret }`.
- `Nullable(T)` — `T?`. Admits `null`, but the inner type still drives coercion,
  so `real? a = 12` stores `12.0`.
- `Union(members)` — kept canonical by `Type::union_of`: flattened,
  deduplicated, at least two members, never containing `Any`/`Null`/`Nullable`
  (null-ness lifts to an outer `Nullable`), and sorted by display name so
  `integer | string` and `string | integer` compare equal.
- `Tuple(members)` — experimental (`LEEK_EXPERIMENTAL_TYPES`); `Array[T0, T1, …]`,
  an ordinary array at runtime with per-position types for checking only.

### 5.2 Assignability

`Type::assignable_to(actual, expected)` answers "may a value of `actual` flow
into a slot declared `expected`?". It is deliberately liberal:

- `Any` on either side, or `Null` on either side, always passes.
- A `Nullable` *target* accepts `null` plus whatever its inner type accepts; a
  `Nullable` *source* is accepted wherever its inner type is (the runtime null
  check covers the rest).
- A union *source* must fit wholly — every member assignable. A union *target*
  accepts a value fitting any one member. The source side is decomposed first,
  so `A|B → A|B` checks member-by-member instead of demanding all of `A|B` fit
  `A`.
- All numeric crosses pass: `Integer`, `Real` and `BigInteger` are mutually
  assignable, with coercion at the assignment.
- Containers match on the outer constructor only — element and key types are not
  compared, and generic-argument variance is out of scope.
- `Tuple` → `Tuple` is position-wise; `Tuple` → `Array<T>` is member-wise (a
  tuple *is* an array). `Array` → `Tuple` is **rejected**: a plain array's length
  and per-position types are unknown, and that strictness is the point.
- Class instances match by name; generic arguments do not affect assignment.
- An un-annotated `Function` and a `FunctionWithReturn` are cross-assignable;
  two annotated ones compare their return types. Parameter mismatches are caught
  at the call site instead.

### 5.3 Joins

`unify_types(a, b)` is the join used for ternary arms, narrowed merges, and
container element types. Equal types collapse, `Integer` + `Real` promotes to
`Real`, `Null` + `T` becomes `T?`, and anything else forms a canonical union.

Inferred joins are **bounded**: a union wider than `MAX_INFERRED_UNION` (4)
collapses to `Any`, which keeps inferred types readable and the structural
recursion cheap. The cap applies to joins only — an explicit annotation keeps
however many members the author wrote.

### 5.4 Generic signatures

User code has no generic syntax, but many builtins are naturally generic
(`first(Array<T>) -> T`, `arrayMap(Array<T>, T -> U) -> Array<U>`). Those are
described in `leek_types::generic` with `GType`, a type *pattern* that may
mention variables. `GenericSig::instantiate` solves the variables against a call
site's concrete argument types and substitutes them into the return pattern,
yielding a plain `Type`.

Generics therefore live only inside signature definitions. They are resolved
during inference and never enter the `Type` enum or reach codegen.

### 5.5 Flow narrowing

`leek_types::checker::narrow` extracts `(positive, negative)` facts from a
boolean condition and binds them in the guarded branch only: `x instanceof T`
narrows `x` to `T`; `x == null` / `x != null` narrow to null and non-null; and
`&&`, `||`, `!`/`not` combine those. Branch checkers push a scope, apply the
facts, and pop it, so a refinement never escapes its branch.

## 6. Where this lives

| Rule | Code |
|------|------|
| File identity | `crates/core/leek-span/src/paths.rs` |
| Include resolution, `Folder` | `crates/middle/leek-resolver/src/folder.rs` |
| Include graph, version-aware scan, `IncludeExpander` | `crates/middle/leek-resolver/src/include_graph.rs` |
| Splicing, pre-declaration, scopes | `crates/middle/leek-hir/src/lower/` |
| Type lattice, assignability, joins | `crates/middle/leek-types/src/ty.rs` |
| Generic signatures | `crates/middle/leek-types/src/generic.rs` |
| Flow narrowing | `crates/middle/leek-types/src/checker/narrow.rs` |
