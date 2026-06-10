# leek-lsp

Stdio launcher for the Leekscript **language server**. The binary is a thin
wrapper around the [`leek-lsp` crate](../../crates/tools/leek-lsp/) — all the
actual server logic lives there.

The server speaks the Language Server Protocol over **stdin/stdout** and is
backed by a salsa incremental database, so per-keystroke re-runs hit cache.
It provides diagnostics, completion, hover, go-to-definition/implementation/
type-definition, references, rename, document/workspace symbols, semantic
tokens, inlay hints, signature help, code actions, formatting (document,
range, and on-type), folding, call/type hierarchy, and more.

## Build & install

```sh
# from the repository root
cargo build -p leek-lsp-bin            # → target/debug/leek-lsp
cargo install --path bins/leek-lsp     # install `leek-lsp` into ~/.cargo/bin
```

(The package is named `leek-lsp-bin` to avoid clashing with the library
crate; the produced binary is `leek-lsp`.)

## Usage

You don't normally run it by hand — an editor does, as a stdio subprocess:

- **VS Code**: the [`editors/vscode/`](../../editors/vscode/) extension spawns
  it; point `leek.server.path` at your build (or have `leek-lsp` on `PATH`)
  and use the *Leekscript: Restart Language Server* command after rebuilding.
- **Neovim**: configure `vim.lsp.config("leek", { cmd = { "leek-lsp" }, … })`;
  see [`editors/nvim/`](../../editors/nvim/) for filetype + syntax support.
- **Any LSP client**: launch `leek-lsp` with stdio transport.

`miku lsp` starts the same server (useful when only `miku` is installed).

## Logging

All server-side logs go to **stderr** (VS Code shows them in the
"Leekscript" output channel). Set `LEEK_LSP_LOG=trace` for per-handler entry
logs.

## Host libraries

Clients can pass `initializationOptions.libraries` (e.g. `["leekwars"]`, or
paths to library-definition files) so host-environment functions — the
leek-wars fight builtins — are recognized by diagnostics and completion. In
VS Code this is the `leek.libraries` setting.
