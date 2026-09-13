# Upstream corpus data

The JUnit manifest is **not** committed: `build.rs` extracts it straight from
the upstream Java sources into `OUT_DIR/upstream_cases.toml` and embeds it at
compile time, so there is no manual extract step and no copy to keep in sync.

`baseline.toml` — per-backend baseline of the **non-passing** outcomes
(`run --save-baseline`). A case the file does not mention was passing, so a
refresh is a few kilobytes of real signal instead of a full ~11k-case pass map.

`reference.tsv` — the official-LeekScript reference dataset (value + ops +
generated Java per case), refreshed with
`cargo run -p leek-test-corpus -- extract-reference` (needs a JDK and the
`official-generator` submodule). `build.rs` regenerates it only when it is
stale and those are available; otherwise the committed copy is embedded.

Run all linked backends (pipeline, native, java):

```bash
cargo run -p leek-test-corpus -- run
```

Inspect failures, grouped by category, with expected vs. actual:

```bash
cargo run -p leek-test-corpus -- failures [pipeline|native|java] [category]
```
