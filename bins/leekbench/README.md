# leekbench

Benchmark harness comparing the workspace's backends against the upstream
reference implementation (the official Java LeekScript). It produced the
numbers in the [top-level README](../../README.md#benchmarks).

Backends compared:

- **rust-native** — this repo's Cranelift JIT,
- **rust-java** — this repo's Java transpiler, run on the upstream runtime
  classes,
- **upstream-java** — the official compiler+runtime, as the reference.

## Build

```sh
# from the repository root
cargo build -p leekbench --release     # benchmarks should use --release
cargo run -p leekbench --release -- <args>
```

## Prerequisites

The native backend needs nothing extra. The two JVM backends need:

- a JDK (`javac` and `java` on `PATH`), and
- the upstream classes compiled at
  `official-generator/leek-wars-generator/leekscript/build/classes`
  (modern Gradle nests them under `java/main`). That means initializing the
  submodules (`git submodule update --init --recursive`) and running the
  upstream Gradle build inside
  `official-generator/leek-wars-generator/leekscript/`. Dependency jars are
  picked up from `~/.gradle/caches`.

No JDK? Skip the JVM backends:

```sh
leekbench program.leek --no-upstream --no-rust-java
```

## Single-file mode

```sh
cargo run -p leekbench --release -- path/to/program.leek --runs 10
```

Runs the program on each backend and prints a table of cold time, warm
median/min/p95, the ratio to the best backend, and the program's result
(so you can see at a glance that all backends agree). Times are inner
program time only — JVM start-up is excluded. Compute-heavy fixtures used
for the README table are checked in under
[`crates/testing/leek-bench/fixtures/`](../../crates/testing/leek-bench/fixtures/).

Useful flags: `--runs N` (default 5), `--lang-version {1..4}` (default 4),
`--no-upstream` / `--no-rust-java` / `--no-native` to drop a backend.

## Corpus mode

`--corpus` iterates over the embedded corpus of test cases extracted from
the pinned upstream test suite, checking each backend's *value* against the
reference's expectation:

```sh
leekbench --corpus --limit 100                  # first 100 equals(...) cases
leekbench --corpus --case-filter string         # only cases matching "string"
leekbench --corpus --fast-java                  # full rust-java correctness sweep
```

`--fast-java` is the batch correctness sweep: it emits every case, compiles
them in one `javac`, and runs them in one JVM — minutes instead of hours.
It checks values only (no timing; native and upstream are skipped).

Other corpus flags: `--corpus-expectation {equals|clean|all}`,
`--include-disabled`, `--manifest <json>` (an external corpus manifest),
`--corpus-lang-version`, and `--work-root` (where scratch files go).

Run `leekbench --help` for the full reference.
