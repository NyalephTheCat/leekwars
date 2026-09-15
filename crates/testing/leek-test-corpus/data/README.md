# Upstream corpus data

The JUnit manifest is **not** committed: `build.rs` extracts it straight from
the upstream Java sources into `OUT_DIR/upstream_cases.toml` and embeds it at
compile time, so there is no manual extract step and no copy to keep in sync.

`baseline.toml` — per-backend baseline of the **non-passing** outcomes
(`run --save-baseline`). A case the file does not mention was passing, so a
refresh is a few kilobytes of real signal instead of a full ~11k-case pass map.

There is no `fmt-known-failures-*.tsv` here any more: the formatter's
known-bad lists (#197) have been worked off to nothing and both suites gate
on a plain assertion instead. See [Formatter ratchet](#formatter-ratchet)
below.

There is no `parse-known-failures.tsv` here any more either: its two rows —
the last open parser gaps under epic R7 — are closed (#351), and the parser
gate is a plain assertion too. See [Parser gate scope and
ratchet](#parser-gate-scope-and-ratchet) below.

`reference.tsv` — the official-LeekScript reference dataset (value + ops +
generated Java per case). **No build touches it** (#148): `build.rs` neither
regenerates nor embeds it, and its readers (`leek-backend-java`'s parity
tests) open this file directly. Refreshing it runs the upstream JVM suite for
minutes, so it is one explicit command:

```bash
cargo run -p leek-test-corpus -- extract-reference   # needs JDK 25 + the submodule
```

That command records a provenance line as the file's first row — the upstream
submodule commit and git's tree hash of `tools/java-emitter/overlay` that
produced the data. `tests/reference_provenance.rs` reads it back and says so
when the checkout has moved on; it stays quiet while the committed file
predates the line, or where the submodule is absent. That replaces the old
mtime comparison, which a fresh clone could resolve either way depending on
checkout order.

One thing a refresh always drags with it: `leek-backend-java`'s
`tests/snapshots/SWITCH_ROWS.txt` identifies its rows by line number in this
file, so any rewrite — the provenance line included — shifts them. Rerun that
test with `UPDATE_SNAPSHOTS=1`, check that only the numbers moved, and commit
the refreshed snapshot together with the dataset.

Run all linked backends (pipeline, native, java-emit):

```bash
cargo run -p leek-test-corpus -- run
```

Inspect failures, grouped by category, with expected vs. actual:

```bash
cargo run -p leek-test-corpus -- failures [pipeline|native|java-emit] [category]
```

## What each column claims

The three columns are checked by different logic, and only one of them is
about values. Read them accordingly:

- **`pipeline`** — compile gate. The program parses, resolves, typechecks and
  lowers to HIR. It says nothing about what the program computes.
- **`native`** — the value check. It runs the program on the Cranelift JIT and
  compares the value, plus the operation count where the expectation carries
  one. Constructs outside the compiled subset are recorded as skips, not
  passes.
- **`java-emit`** — emit-only. It proves the Java emitter turned this HIR into
  a file without panicking; it never compiles or executes that file, so it
  cannot see a miscompile. A near-100% `java-emit` column is what the column
  measures, not evidence that the Java backend is correct. Values are verified
  by `native`, and against a real JVM by `leek-bench`'s `run_fast_java_corpus`
  and `leek-backend-java`'s parity tests — neither of which is wired into this
  corpus yet (#70).

## Why `pipeline` and `java-emit` read the same

`pipeline` and `java-emit` currently read `total = 12254, pass = 12212,
fail = 0, skipped = 18` — the same numbers, and that is the *expected* shape
rather than a baseline saved while the two shared a code path: a case that
compiles for `pipeline` emits for `java-emit`, so the two columns can only
diverge once the emitter itself breaks on HIR the frontend accepted. Do not
read identical columns as "the baseline is broken", and do not add a check
that requires them to differ — that is a check that requires the compiler to
be broken. What separates the columns is their check *logic*, pinned in
`leek-test-driver/tests/safety_net_honesty.rs`.

`native` carries the value check, so it is the column that moves on its own:
`fail = 0, skipped = 30` today. Its skips are constructs outside the compiled
subset, not wrong answers.

The corollary is that the `pipeline` / `java-emit` summaries carry no
per-backend signal. The numbers that would carry it — a real JVM value check
for the Java backend — live outside this corpus (see `java-emit` above, and #70).

### What the baseline holds now

Nothing: every case every backend runs, passes, so `baseline.toml` records
only skips. That is what makes it a ratchet rather than a scoreboard — the
next change cannot add a failure without the file moving.

It arrived there from the `official-generator` bump to `v3.00`, which moved
the nested `leekscript` submodule with it and grew the extracted suite from
11005 cases to 12254. That landed 141 compile-gate and 159 native failures at
once, none of them regressions — every one was a case this toolchain had
never run. Working them off is what the branch that bumped the generator did,
feature by feature: the `a?[b]` optional access operator; the diagnostics
v3.00 added (function redefinition, duplicate globals, a foreach iterator
shadowing a class field, duplicate `switch` defaults, dead code after a
returning `switch`, `big_integer` DoS guards, `final` static-field
assignment, incompatible default-parameter types); upstream's `ConstantFolder`
and its switch optimizer; passive weapon effects; and the typed-slot
conversions a declared type performs on every write, parameter and return.

Skips are the remaining honest gap: a case the driver cannot model
(`skipped_unknown`) or one upstream itself disables (`skipped_disabled`).
They are not failures, and they are not passes either — nothing checks them.

## Formatter ratchet

`tests/fmt_roundtrip.rs` formats every corpus case and every upstream `.leek`
AI file twice and checks that `leek_fmt::format_source_checked` accepts the
output and that the second run is a no-op. Its first run found **289 of 11005
corpus cases and 16 of 101 AI files broken** — the safety net's first contact
with real code, and several of those are the formatter changing what a program
means (`not true` printed as `nottrue`, `(x -> e)(a)` printed as
`(x) -> e(a)` — the latter fixed in #416).

`fmt-known-failures-corpus.tsv` and `fmt-known-failures-ai.tsv` held those
failures, one `id<TAB>kind<TAB>detail` row each, and each suite gated on the
**diff** against its own file: an id that broke and was not listed failed the
build, a listed id that passed again was reported so the file shrank, and a
reworded detail was information. An empty or missing file is an error, not a
pass — see `src/fmt_ratchet.rs` for why.

Three rounds of fixes worked both files off completely:

| Fixed | Corpus rows | AI rows |
|---|---|---|
| — (first run) | 289 | 16 |
| a separator wherever two adjacent tokens would re-lex as one (#412), `format_binary` emitting every operator token so `not in` keeps its `in` (#413), `format_class_body` emitting a stray modifier instead of dropping it (#414) | 141 | 6 |
| `format_lambda` keeping the parentheses that wrap a whole `(x -> e)` lambda rather than peeling them off its callee (#416), and the bracketed-list printers falling back to verbatim output instead of synthesizing a closer the node never had (#418) | 23 | 3 |
| no formatter printing a delimiter its node does not have (#415, #417), nothing written into a token that runs to end of file (#417, #419), and annotations reaching a fixed point (#420) | 0 | 0 |

**So both files are deleted and neither suite has a ratchet any more.** A
ratchet holding nothing gates on nothing, so each suite is back to the plain
assertion its own error message prescribes: every one of the 11005 corpus
cases and 101 upstream `.leek` files must format safely and idempotently, and
a failure is a formatter regression to fix — never a row to add back.

No row was ever accepted behaviour, and none was ever closed by editing its
detail: each one came off because the bug behind it was fixed, with the one
exception called out below. The machinery is still here — `src/fmt_ratchet.rs` and its tests
in `tests/fmt_ratchet.rs` — for the next defect class too large to fix in the
change that finds it; wiring a suite back to it means restoring the `gate`
call and the `LEEK_FMT_WRITE_KNOWN_FAILURES` write path in
`tests/fmt_roundtrip.rs` (see this file's history for the shape).

The signatures are recorded so a returning row is recognised for what it is:
`` `KwNot` becomes `nottrue` `` and friends (#412), `` `KwIn` becomes … ``
(#413), `` `KwStatic` becomes `Kw…` `` (#414), `` `:` becomes `]` `` /
`` `..` becomes `]` `` (#415), `enter CallExpr becomes enter LambdaExpr`
(#416), `` `<end of file>` becomes … `` and `` `"` becomes `"\n` `` (#417),
`` `KwVar` becomes `>` `` (#418), `would drop 1 comment(s): /*…` (#419),
`not-idempotent … @pure  @unused` (#420).

**One row disappeared as a side effect rather than a fix, and that coverage
is gone.** `euler/pe025.leek` (#418) lost its row because the missing-closer
rule made the formatter round-trip `while |n1.string()| < 1000 {` instead of
rewriting it — the safety net accepts the file now, but the file is still
mis-parsed (there is no `|x|` length-operator production in the grammar, so
the `|` lands in an `ErrorNode` and `< 1000 {…}` becomes a set literal) and it
is still laid out wrongly. Adding that production is a separate parser
feature, and no row watches it any more.

## Parser gate scope and ratchet

`tests/parser_fixtures.rs` asserts two things about the 101 upstream `.leek`
files, at two different scopes, and the difference is the point:

- **every** file round-trips byte for byte through the green tree. Not
  scoped, not negotiable — the formatter and the LSP see whatever a user
  opens;
- the files **upstream's own JUnit suite runs** parse with no lexer, pragma
  or parser diagnostic at all.

The second one used to be asked of all 101, and reported *38 failures* — on
`main`, through the gate, for as long as anyone had been reading it. Most of
those 38 are not gaps. The fixture tree belongs to the standalone `leekscript`
language submodule, not to the Leek Wars generator's AI corpus, and a good
part of it is written in language the Leek Wars dialect does not have:
`1m`/`5m` bignum literals (`code/pow5.leek`, `code/fact1000.leek`,
`code/primes_gmp.leek`, `code/product_*.leek`), `match` with a `..` wildcard
arm (`code/match.leek`), `let` declarations (`code/array.leek`,
`code/fibonacci_v12.leek`), `$`-prefixed dynamic operators, `{k: v}` object
literals. "Close the parser gap" is not a coherent goal for any of them.

Upstream has already decided which fixtures it stands behind, one call site at
a time, in `src/test/java/test/Test*.java`: `DISABLED_file(…)` and
commented-out lines switch a fixture off, live `file(…)` / `file_v1(…)` /
`file_v2_(…)` / `file_v3(…)` / `file_v4_(…)` calls switch it on. `build.rs`
extracts exactly those call sites (`src/extract.rs`, the same scan that
produces the JUnit manifest) into `OUT_DIR/enabled_fixtures.txt`, and
`leek_test_corpus::upstream_enabled_fixtures()` reads them back — **45 of
101** today. Deriving it rather than listing it here is what makes a submodule
bump move the scope instead of rotting it, and
`the_enabled_fixture_set_is_derived_and_plausible` fails if the derived set
ever names a file that does not exist or comes back implausibly small (an
uninitialised submodule embeds an empty set, which would gate on nothing).

All 45 parse cleanly, so the gate is a plain assertion with **no allow-list**.
It briefly had one — `parse-known-failures.tsv`, a ratchet holding two
fixtures, both of which turned out to be this toolchain disagreeing with the
reference implementation rather than dialect it does not target:

| Fixture | Was | The divergence, and the fix (#351) |
|---|---|---|
| `code/french.leek` | `W0005` ×1 | the file ends inside an unterminated `/* …`. Upstream's `LexicalParser.tryParseComments` runs to end of input and says nothing, so the warning was a false positive on valid code; `W0005` is retired. |
| `code/french.min.leek` | `E0100` ×16 | minified LeekScript omits the comma between call arguments and between array elements (`split('…' ' ')`, `[T ' ' x[d] …]`). Upstream's call-argument loop, `readArray` and `readMap` skip a `VIRG` only if one is there; the parser does the same now. |

With both closed the list had no rows, and a ratchet with nothing in it is not
a passing gate but a missing one — so the file went, and
`tests/parser_fixtures.rs` went back to the plain assertion a ratchet is only
ever a detour from. The machinery stays in `src/parse_ratchet.rs` with its own
tests, the way `fmt_ratchet` did after #197, for the next parser gap too large
to close in the change that finds it. Until one turns up, a failing fixture is
a parser bug and reads as one.

## CI

`.github/workflows/corpus.yml` runs the whole thing nightly and on PRs that
touch the compiler; a regression against `baseline.toml` fails that workflow.
A failing run uploads the baseline it would have written as an artifact —
download and commit it verbatim, because `upstream_suite.rs` checks the
committed file is byte-identical to what `--save-baseline` produces.
