# Upstream test manifest

`upstream_cases.toml` — the extracted JUnit suite (~10k cases). `build.rs`
extracts it straight from the upstream Java sources and embeds it at compile
time, so there is no manual extract step.

`baseline.toml` — per-backend pass/fail map (`run --save-baseline`).

Run all linked backends (pipeline, interp, java):

```bash
cargo run -p leek-test-corpus -- run
```

Inspect failures, grouped by category, with expected vs. actual:

```bash
cargo run -p leek-test-corpus -- failures [pipeline|interp|java] [category]
```
