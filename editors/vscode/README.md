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
| `leek.miku.path` | `miku` | Path to the `miku` executable, used by the run/test commands and the scenario lenses. |
| `leek.trace.server` | `off` | LSP trace level (`off`/`messages`/`verbose`). |
| `leek.libraries` | `[]` | Host function libraries: a built-in name (`"leekwars"` for the fight builtins) or a path to a library-definition file. Restart the server after changing. |

Server logs appear in the **Leekscript** output channel. Warnings and errors
— a refused "Format Document", a project that failed to index, a handler that
panicked — are sent to the channel by the server itself, so a bug report can
quote them; everything quieter goes to the server's stderr, which the client
also captures.

By default only lifecycle events are logged. Set `LEEK_LSP_LOG` in the
server's environment for more: a bare level (`trace`, `debug`, `info`,
`warn`, `error`) applies to the server, and anything else is read as a
`tracing` filter directive list (`leek_lsp=debug,salsa=off`). `trace` turns
on the per-handler logs, including the per-edit diagnostics lines. An
unparseable value falls back to the default rather than failing to start.

There is no VS Code setting for this yet — see
[#309](https://github.com/NyalephTheCat/leekwars/issues/309), which should
contribute a `leek.server.logLevel` property and forward it as `LEEK_LSP_LOG`
in the server's `env`.

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

## Running fights and tests from the editor

Open a fight scenario (a `.toml` with `[[entities]]`, or one that inherits
them with `extends = "…"`) and the extension draws code lenses on it:

- **Run fight** at the top — `miku fight <scenario>`.
- **Run matrix** next to it, but only when the scenario carries a
  `[testing]` table. Without one every sweep axis is empty and
  `--mode matrix` collapses to a single fight, so there would be nothing
  to offer.
- **Debug `<ai>` in this fight** over each `[[entities]]` block with an
  explicit `ai = "…"` — starts a `leek-dap` session with that entity's
  `id` as `fightEntity`, breaking at the AI's first statement. This is
  the one-click form of the "Debug hero inside the duel" config in
  [`examples/fight/.vscode/launch.json`](../../examples/fight/.vscode/launch.json).

The lens sniff is deliberately conservative: an entity that names its AI
indirectly (`leek = "leeks/hero.toml"`) or inherits it through `extends`
gets no debug lens, because the file alone does not say which program to
launch. Run the fight and use a hand-written launch config for those.

The same actions, plus **Leekscript: Run Tests** (`miku test`), are in the
command palette. All of them shell out to `miku`, which the extension does
not bundle — install it (`cargo install --path bins/miku`) or point
`leek.miku.path` at a build. Fights over the leek-wars builtins need the
host library, so the `leek.libraries` setting is forwarded as `--library`
flags, exactly as on the command line.

Runs happen as VS Code tasks in a dedicated terminal rather than behind a
progress spinner: a matrix sweep plays its cells one at a time and can
take minutes, so it has to stay readable and cancellable. Failures from
`miku test` are parsed into the Problems panel by the `$miku-test`
matcher, which resolves paths relative to the workspace folder.

There is no Test Explorer: `miku test` has no machine-readable result
stream (only JUnit XML, written once at the end), and scraping its
`PASS`/`FAIL` lines into a tree would be a stub rather than a feature.

**Leekscript: Show Complexity Analysis** runs the server's `leek.analyze`
over the active `.leek` file and lists every function with its big-O and
operation formula — the same records `miku analyze` prints.

## Layout

```
package.json                 manifest: language, grammar, debugger, settings, commands, tasks
src/extension.ts             activation: LSP client, DAP factory, restart / run / debug commands
src/scenario.ts              pure scenario sniff behind the TOML code lenses
src/scenario.test.ts         its unit tests (`npm test`, no VS Code host needed)
syntaxes/leek.tmLanguage.json   TextMate grammar (keep in sync with editors/nvim/syntax/)
language-configuration.json  brackets, comments, auto-closing pairs
```
