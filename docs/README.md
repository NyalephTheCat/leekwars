# Documentation

Project documentation for the Rust LeekScript toolchain.

| Document | What it covers |
|----------|----------------|
| [`architecture.md`](architecture.md) | Crate layers, the compilation pipeline, the fight simulator, and how the pieces fit together. Start here to find your way around the codebase. |
| [`pipeline.md`](pipeline.md) | The query layer: the salsa inputs, every tracked query, what an edit invalidates, and what is left of the two-orchestration-models migration. |
| [`grammar.md`](grammar.md) | The formal LeekScript grammar (ECMA-262-inspired notation). |
| [`semantics.md`](semantics.md) | Front-end semantics: file identity, `include` expansion, version resolution, scope and boundary rules, and the type lattice. |
| [`lsp.md`](lsp.md) | The language server's design: the workspace and its salsa DB, per-program symbol scoping, threading and panic containment, the diagnostic set, and the logging seam. |
| [`java-backend.md`](java-backend.md) | The Java backend spec: emission modes, output shape, identifier mangling, the op-cost model, v1 boxing, the parity harness, and the known byte-parity gaps. |
| [`leekscript-backend.md`](leekscript-backend.md) | The LeekScript source backend: the single-self-contained-file contract, include splicing, one declaration per global, what `Options::version` does, and the equivalence harness. |

See also:

- [`../README.md`](../README.md) — project overview, quick start, `miku`
  commands, and backend benchmarks.
- [`../CONTRIBUTING.md`](../CONTRIBUTING.md) — dev setup, the quality gate,
  code style, and the PR flow.
- [`../CODE_OF_CONDUCT.md`](../CODE_OF_CONDUCT.md) — community expectations.
- Per-binary guides under [`../bins/`](../bins/) (`miku`, `leekc`, `leek-lsp`,
  `leek-dap`, `leekbench`) and editor integrations under
  [`../editors/`](../editors/).
