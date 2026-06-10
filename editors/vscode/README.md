# Leekscript for VS Code

Language support for Leekscript: TextMate syntax highlighting, full language
features via [`leek-lsp`](../../bins/leek-lsp/) (diagnostics, completion,
hover, rename, semantic tokens, formatting, …), and debugging — including
debugging an AI **inside a running fight** — via
[`leek-dap`](../../bins/leek-dap/).

## Prerequisites

- **Node.js** (18+) and `npm` — to build the extension itself.
- The **`leek-lsp` and `leek-dap` binaries** — the extension spawns them as
  subprocesses; it does not bundle them. From the repository root:

  ```sh
  cargo install --path bins/leek-lsp
  cargo install --path bins/leek-dap
  ```

  or `cargo build -p leek-lsp-bin -p leek-dap-bin` and point the
  `leek.server.path` / `leek.debugAdapter.path` settings at
  `target/debug/leek-lsp` / `target/debug/leek-dap`.

## Building the extension

```sh
cd editors/vscode
npm install            # fetch dependencies (esbuild, vsce, …)
npm run compile        # bundle src/extension.ts → dist/extension.js
```

`npm run watch` does the same with sourcemaps and rebuilds on every change.

## Installing it into VS Code

Package a `.vsix` and install it:

```sh
npm run package                                   # compile + vsce package → leek-0.0.1.vsix
code --install-extension leek-0.0.1.vsix
```

For a development loop without packaging, run an Extension Development Host
against the compiled output:

```sh
npm run compile
code --extensionDevelopmentPath="$PWD" /path/to/some/leek-project
```

Opening any `.leek` file activates the extension and starts the language
server. After rebuilding `leek-lsp`, run the **Leekscript: Restart Language
Server** command (it restarts the server in place — no window reload).

## Settings

| setting | default | meaning |
|---|---|---|
| `leek.server.path` | `leek-lsp` | Path to the language-server executable. |
| `leek.debugAdapter.path` | `leek-dap` | Path to the debug-adapter executable. |
| `leek.trace.server` | `off` | LSP trace level (`off`/`messages`/`verbose`). |
| `leek.libraries` | `[]` | Host function libraries: a built-in name (`"leekwars"` for the fight builtins) or a path to a library-definition file. Restart the server after changing. |

Server logs appear in the **Leekscript** output channel; set
`LEEK_LSP_LOG=trace` in the server's environment for per-handler logs.

## Debugging `.leek` programs

Press **F5** on a `.leek` file — with no `launch.json`, the extension fills
in a default config that debugs the active file. Breakpoints, stepping,
stack traces and variable inspection work against the native backend; see
the [`leek-dap` README](../../bins/leek-dap/README.md) for capabilities and
all launch options.

To debug an AI inside a fight, add a `scenario` to the launch config (the
"Leekscript: Debug AI in fight" snippet scaffolds it) — breakpoints then
fire during the turn loop. A complete example with ready-made launch
configs lives in [`examples/fight/`](../../examples/fight/).

## Layout

```
package.json                 manifest: language, grammar, debugger, settings, commands
src/extension.ts             activation: LSP client + DAP factory + restart command
syntaxes/leek.tmLanguage.json   TextMate grammar (keep in sync with editors/nvim/syntax/)
language-configuration.json  brackets, comments, auto-closing pairs
```
