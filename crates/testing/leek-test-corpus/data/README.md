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

## Why all three columns look the same

Every column currently reads `total = 11005, pass = 10153,
pass_expected_error = 828`, with zero failures and zero unknown skips. That is
a real result, not a baseline saved while the backends shared a code path — a
run on today's HEAD reproduces the numbers recorded three months ago exactly,
column for column.

It is also the *expected* shape while `native` is at 100%: a case native
compiles, runs and value-checks necessarily compiles for `pipeline` and emits
for `java-emit`, so the three columns can only diverge once something fails.
Do not read identical columns as "the baseline is broken", and do not add a
check that requires them to differ — that is a check that requires the
compiler to be broken. What separates the columns is their check *logic*,
pinned in `leek-test-driver/tests/safety_net_honesty.rs`.

The corollary is that these summaries carry no per-backend signal today. The
numbers that would carry it — a real JVM value check for the Java backend —
live outside this corpus (see `java-emit` above, and #70).

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

## CI

`.github/workflows/corpus.yml` runs the whole thing nightly and on PRs that
touch the compiler; a regression against `baseline.toml` fails that workflow.
A failing run uploads the baseline it would have written as an artifact —
download and commit it verbatim, because `upstream_suite.rs` checks the
committed file is byte-identical to what `--save-baseline` produces.
