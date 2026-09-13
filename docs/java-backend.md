# Java backend

`crates/backends/leek-backend-java` reads typed/resolved HIR and produces the
textual contents of one `AI_<id>.java` file plus a `.lines` sidecar mapping
generated Java lines back to LeekScript source lines. The upstream Java
reference it mirrors is vendored as the `official/leek-wars` submodule
(`IACompiler`, `JavaWriter`, `LeekExpression`, `LeekFunctions`); when this
document and that code disagree, the submodule wins and this document is the
bug.

Entry point: `emit(&HirFile, &Options)`, with `emit_exact` / `emit_clean`
convenience wrappers presetting the option struct.

## 1. Pipeline position

`leek-backend-java` sits in the backends layer: it consumes `leek-hir` (plus
`leek-charge` in clean mode) and depends on nothing above it. See
[`architecture.md`](architecture.md) for the full crate layering.

The emitter is split across `src/emit/` submodules — `mod.rs` orchestrates and
holds the shared `Emitter` state, with `expr.rs`, `stmt.rs`, `call.rs`,
`class.rs`, `lambda.rs`, `literals.rs` and `switch.rs` handling one construct
family each.

## 2. Emission modes

`Options::mode` selects between two shapes of output. Everything else in
`Options` (language version, AI id, environment catalog, base class, source
path) applies to both.

### 2.1 Exact

`Mode::Exact` mirrors the upstream reference's emission shape so the output can
be diffed against captured goldens:

- full `u_` / `g_` / `f_` mangling (§4), never dropped;
- per-statement `ai.ops(1)` ticks and `ops(value, n)` wrappers (§5);
- runtime-comparison `switch` lowering (chained `equals_equals` tests) rather
  than a native Java `switch`;
- `add(...)` / `sub(...)` / `mul(...)` helpers for arithmetic whose operands
  are not statically numeric;
- no dead-code elimination — statements after a definite `return` are still
  emitted, because the reference emits them.

Byte-for-byte parity is not reached yet; §9 lists what is still missing.

### 2.2 Clean

`Mode::Clean` is the readable / optimized variant. It opts into
liberalizations the reference does not take:

- the `u_` / `f_` prefix is dropped when the bare name collides with neither a
  Java keyword nor a runtime member (§4);
- per-block static op cost is folded into a single `charge(n)` at block entry
  by the `leek-charge` pass (`Options::with_charge`) instead of per-statement
  ticks;
- unreachable statements after a definite `return` / `break` / `continue` are
  dropped (`Options::dead_code_elim`);
- a native Java `switch` is emitted when every case label is a constant
  (`Options::native_switch`);
- output is indented for readability — cosmetic only.

Clean mode is not diffed against the goldens; its contract is "compiles and
behaves identically", not "looks identical".

## 3. Output shape

One compilation unit per AI:

```java
public class AI_<id> extends <base_class> {
    // global fields + per-global `g_init_<x>` flags
    // class handles and `new_<C>(args)` construction helpers
    // `__shadows` map, only when the source reassigns a builtin name
    public AI_<id>() throws LeekRunException { super(<n>, <version>); ... }
    public void staticInit() throws LeekRunException { ... }
    public Object runIA(Session session) throws LeekRunException { ... }
    // user functions, lambda factories, class members
}
```

`base_class` defaults to `AI` (the LeekScript runner base) and is set to
`EntityAI` for leek-wars-generator fight AIs, matching the generator's
`LeekScript.compileFileContext(…, "…fight.entity.EntityAI", …)`.

`super(n, version)` carries the *instruction* count of the main block, mirroring
upstream `MainLeekBlock.mInstructions.size()`: top-level main-block instructions
only, with an `if`/`else` registering as two sibling instructions. Nested
loop/switch bodies are separate blocks and do not contribute.

`staticInit` runs two passes over the user classes — every
`createStaticClass_<C>()` first, then every `initClass_<C>()` — so one class's
static initializer can reference another class's statics.

The `.lines` sidecar is `<javaLine> <fileIndex> <leekLine>` per line, with
`fileIndex` always `0` for single-file emission. It is what makes JVM stack
traces round-trip to LeekScript source.

## 4. Identifier mangling

All of it lives in `src/mangle.rs`.

| Kind | Exact mode | Clean mode |
|------|-----------|-----------|
| local / parameter / `for`-bound | `u_<name>` | bare `<name>` unless reserved |
| top-level function | `f_<name>` | bare `<name>` unless reserved |
| global variable | `g_<name>` | `g_<name>` (always) |
| user class | `u_<C>` | `u_<C>` (always) |
| method | `u_<name>` | `u_<name>` (always) |
| constructor param bind | `p_<name>` | as exact |

Static class fields are not Java fields: they are registered on the
`ClassLeekValue` by their *source* name (`addStaticField(this, "<name>", …)`)
and read back through the runtime, so they are never mangled.

Globals stay prefixed in clean mode because they share the generated class's
scope with synthetic helpers. User classes stay prefixed because the prefix is a
*runtime contract*, not just disambiguation: the runtime recovers the leek class
name from the Java class name with `getSimpleName().substring(2)` when
formatting a visibility-denial error, so a bare `Cat` would make that
`substring(2)` throw.

"Reserved" in clean mode means either a Java keyword / restricted identifier
(JLS §3.9) or a member that lives on the generated `AI` superclass (`add`,
`ops`, `clone`, `runIA`, `staticInit`, …) that a bare user name would shadow.
Exact mode never needs the check: a prefixed name cannot collide.

**Character escaping.** Every character that is not `[A-Za-z0-9_]` escapes to
`_uXXXX`, one group per UTF-16 code unit. `mangle::safe_chars` is the single
implementation; both the prefixed spellings above and every *synthesized*
identifier route their stem through it:

- `g_init_<x>` — the per-global "already initialized" flag
  (`mangle::global_init_flag`);
- `createStaticClass_<C>` / `initClass_<C>` — the `staticInit` hooks
  (`mangle::create_static_class`, `mangle::init_class`);
- `p_<param>` and `<class>_<method>_<arity>` — emitted via
  `emit::sanitize_ident`, which is `safe_chars` without a prefix, because those
  spellings supply their own.

Skipping the escape anywhere here emits invalid Java for a non-ASCII
identifier, so new synthesized names must go through `mangle` too.

## 5. Op-cost model

The runtime charges "operations" and kills an AI that exceeds its budget, so op
counts are observable behaviour and must match upstream, not merely be
plausible.

Two mechanisms, matching the reference:

- **per-statement ticks** — `ai.ops(1)` for each executed statement, folded
  into the value-producing expression where one exists;
- **static expression cost** — `ops(value, n)`, where `n` is
  `emit::expr_op_cost`, mirroring `LeekExpression.computeOperations` and the
  cost table in `LeekValueType.java`.

Cost table, as implemented by `HIR BinaryOp::op_cost` / `UnaryOp::op_cost`:

| Expression | Static cost |
|-----------|-------------|
| literal, variable read, cast, call itself | 0 |
| `&&`, `\|\|` | 0 at this level — operand costs are distributed into per-operand `ops(...)` wrappers so a short-circuited right side is not charged |
| most binary operators | 1 |
| `*` | 2 (`MUL_COST`) |
| `/`, `%`, `\` | 5 (`DIV_COST` / `MOD_COST`) |
| `**` | 40 (`POW_COST`) |
| postfix `++` / `--` | 1 + operand |
| array literal | 2 per element |
| map / set literal | 2 per entry |

Builtin calls add a per-call cost from the generated `leek_builtins::op_cost_emit`
table (upstream `LeekFunctions.method(name, _, cost, _)`); user-function calls
add nothing at the call site because the callee body ticks its own statements.
A receiver method call `recv.m(args)` adds `1 + cost(recv)`, mirroring
`LeekObjectAccess.analyze`, except for builtin-class receivers like
`Integer.parse(...)`. Indexing into an *array literal* and calling the result
(`[…][i](args)`) charges the literal's element cost at the call site, since the
literal in callee position gets no `ops(...)` wrapper of its own.

Clean mode replaces all of this with one folded `charge(n)` per block, computed
by `leek-charge` from the same tiers (`crates/middle/leek-charge`), which is why
the op-cost tiers are centralized there and in `leek-builtins` rather than
duplicated per backend.

## 6. v1 boxing

At v1 every local is a `Box` in the upstream runtime, which leaks into
observable semantics. The emitter reproduces the parts the corpus exercises:

- **`@`-by-ref parameters.** A local passed as a mutated `@` argument is
  declared as a runtime `Box` so the callee — or a closure it returns — aliases
  and mutates the caller's variable. Reads emit `.get()`, writes route through
  `Box` methods. `Emitter::ref_boxes` holds the set, seeded by
  `caller_box_locals`.
- **Box-returning callees.** A v1 function whose body returns a plain variable
  returns a `Box` upstream, directly or transitively. `var x = f()` then has to
  mirror upstream's clone-if-box with a `copy(...)` wrapper. `v1_box_returners`
  computes the closure over both named functions and function-valued variables.
- **Captured locals.** A local captured by a nested lambda is heap-boxed as an
  `Object[]`, with every read/write going through `[0]`. Populated once by
  `collect_boxed_locals`.
- **Self-recursive anonymous functions.** `var f = function(){ … f() … }` is
  legal at v1; the factory takes an `Object[] _self_box` and the body emits
  `_self_box[0]` for its self-references.

At v2+ all of the above is inert: the sets are empty and no `Box` is emitted.

## 7. Host environment

`Options::environment` carries a host builtin catalog (combat/game functions
like `getCell`, `moveToward`). When set, a call to one of its functions emits
the generator-compatible dispatch (`EntityClass.getCell(ai, …)`) plus the
matching `import`. With `None`, only language builtins are known and an unknown
name falls back to a bare `name(...)` call.

## 8. Parity harness

`tests/parity.rs` drives three things:

1. **Shape assertions** — every fixture under `tests/fixtures/inputs/*.leek`
   must produce the same class shell, constructor signature, `runIA(Session)`
   signature and user-function declarations as the captured reference output in
   `tests/fixtures/golden/<name>.java`. Goldens are produced by
   `tools/java-emitter/build/leekscript-emitter.jar`, a thin driver around the
   upstream `IACompiler`.
2. **Diff snapshots** — the full unified diff between exact-mode output and the
   golden lands in `tests/snapshots/<name>.exact.diff` on every run. Today it is
   a tracking artifact, not a hard gate.
3. **JVM cross-check** — the corpus is emitted, compiled with `javac` and run,
   comparing both returned value and op count against the reference. Ratchets
   guard the pass rates (value parity, op parity, and a zero-tolerance ceiling
   on compile/run errors); the per-case breakdown is written to
   `tests/snapshots/JVM_PARITY.txt`. Bump the ratchets up as fixes land so
   regressions cannot sneak back in.

Note that the JVM cross-check rewrites tracked snapshot files whose op counts
are non-deterministic (anything driven by `randInt`); `git checkout` them after
a local run unless updating them is the point of the change.

## 9. Known gaps

Byte parity is a Phase-3 goal. What stands between here and there:

- **Function emission order.** The reference orders functions by Java `HashMap`
  iteration over `MainLeekBlock.mFunctions` — implementation-defined and not
  reproducible from Rust without reimplementing the JVM's hash semantics. The
  `byte_parity_09_multi_func` test is kept `#[ignore]`d as a marker.
- **Block-bodied lambdas cannot see outer locals.** Emit needs to outline them
  into top-level helper methods.
- **Assignment to a builtin / function / class name** (`count = 1; return count`)
  needs a HIR-level rewrite that shadows the name with a local; the `__shadows`
  map only covers the read side.
- **v1–v3 receiver-method calls into `LegacyArrayLeekValue`** methods that do
  not exist on it (`arrayMap`, `arrayFilter`, `arrayFind`, …). Upstream emits
  per-call-site `Array_<name>_<sig>` helpers via
  `JavaWriter.writeGenericFunctions`; we do not yet.
- **Default parameter values** are not lowered into call-site null-fill or
  synthesized overloads.
- **Index l-value chains with promote-on-write semantics** — `t[i][j] = v`
  where the inner array morphs into a sparse map at v1–v3.
- **Bit-XOR `^` means POWER at v1**, not XOR; we lower it as XOR.
- **Exotic reference paths** — narrowing casts on boxed locals and
  `rfunction_` reassignable-function fixups emit a structurally valid
  approximation rather than the reference's exact shape.
