# Upstream corpus data

The JUnit manifest is **not** committed: `build.rs` extracts it straight from
the upstream Java sources into `OUT_DIR/upstream_cases.toml` and embeds it at
compile time, so there is no manual extract step and no copy to keep in sync.

`baseline.toml` — per-backend baseline of the **non-passing** outcomes
(`run --save-baseline`). A case the file does not mention was passing, so a
refresh is a few kilobytes of real signal instead of a full ~11k-case pass map.

`fmt-known-failures-corpus.tsv`, `fmt-known-failures-ai.tsv` — the
formatter's known-bad list, one sorted row per failing id (#197). See
[Formatter ratchet](#formatter-ratchet) below.

`reference.tsv` — the official-LeekScript reference dataset (value + ops +
generated Java per case), refreshed with
`cargo run -p leek-test-corpus -- extract-reference` (needs a JDK and the
`official-generator` submodule). `build.rs` regenerates it only when it is
stale and those are available; otherwise the committed copy is embedded.

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
`(x) -> e(a)`).

`fmt-known-failures-corpus.tsv` and `fmt-known-failures-ai.tsv` hold those
failures, one `id<TAB>kind<TAB>detail` row each, and the test gates on the
**diff**: an id that breaks and is not listed fails the build, a listed id
that now passes is reported so the file shrinks, and a reworded detail is
information. An empty or missing file is an error, not a pass — see
`src/fmt_ratchet.rs` for why.

**Every row is an open bug.** Fixing one means deleting its row; nothing here
is accepted behaviour, and nothing here is a reason to weaken the check that
found it. The rows group into nine defect classes:

| Row detail looks like | Defect | Issue |
|---|---|---|
| `` `KwNot` becomes `nottrue` ``, `` `KwStatic` becomes `staticArray` ``, `` `12` becomes `12b` ``, `` `<` becomes `<=` `` | adjacent tokens printed with no separator, re-lexing as one | #412 |
| `` `KwIn` becomes … `` | `not in` loses its `in` | #413 |
| `` `KwStatic` becomes `Kw…` `` | `static` dropped from a class member | #414 |
| `` `:` becomes `]` ``, `` `..` becomes `]` `` | `a[i:j]` / `[a..b]` shredded into separate statements | #415 |
| `enter CallExpr becomes enter LambdaExpr` | the paren making a lambda the callee is peeled (114 rows) | #416 |
| `` `<end of file>` becomes … ``, `` `"` becomes `"\n` `` | tokens invented past the end of the file | #417 |
| `` `KwVar` becomes `>` `` (`euler/pe025.leek`) | the `\|x\|` length operator | #418 |
| `would drop 1 comment(s): /*…` | unterminated block comment grows a line per pass | #419 |
| `not-idempotent … @pure  @unused` | annotations gain a space per pass | #420 |

Regenerate both files after fixing something (needs the upstream submodule):

```bash
LEEK_FMT_WRITE_KNOWN_FAILURES=1 cargo test -p leek-test-corpus --test fmt_roundtrip
```

Regenerating is for *shrinking* the files. If the gate reports an id as newly
broken, that is a formatter regression to fix — writing it into the file is
how a real corruption gets baked in.

## CI

`.github/workflows/corpus.yml` runs the whole thing nightly and on PRs that
touch the compiler; a regression against `baseline.toml` fails that workflow.
A failing run uploads the baseline it would have written as an artifact —
download and commit it verbatim, because `upstream_suite.rs` checks the
committed file is byte-identical to what `--save-baseline` produces.
