# leek-fuzz — cargo-fuzz targets

Standalone (nightly/libfuzzer) crate, excluded from the stable workspace.

## Targets
- **parse_roundtrip** — the parser never panics and the CST losslessly
  round-trips arbitrary input (the in-tree `leek-parser` `fuzz_roundtrip` test
  is the deterministic, regression-pinned version of this).

## Run
```sh
cargo +nightly fuzz run parse_roundtrip fuzz/seeds/parse_roundtrip
```
A crash writes a reproducer under `fuzz/artifacts/`; replay it with
`cargo +nightly fuzz run parse_roundtrip fuzz/artifacts/parse_roundtrip/<file>`.
`corpus/` and `artifacts/` are git-ignored (local working state); committed seed
inputs live in `seeds/`.
