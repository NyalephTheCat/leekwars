# leek-dap

Stdio launcher for the Leekscript **debug adapter**. The binary is a thin
wrapper around the [`leek-dap` crate](../../crates/tools/leek-dap/) — all the
adapter logic lives there.

It speaks the Debug Adapter Protocol over **stdin/stdout**. The debuggee is
the **native (Cranelift) backend**: on launch the adapter compiles the
program in debug mode (no optimization, per-statement safepoints, DWARF) and
runs it in-process. Supported today: line breakpoints, `stopOnEntry`,
step in/over/out, multi-frame stack traces, and per-frame local-variable
inspection. Known gap: a breakpoint on a line that lowers to only a
terminator (a bare `return x`) won't fire, since safepoints are
per-statement.

## Build & install

```sh
# from the repository root
cargo build -p leek-dap-bin            # → target/debug/leek-dap
cargo install --path bins/leek-dap     # install `leek-dap` into ~/.cargo/bin
```

(The package is named `leek-dap-bin` to avoid clashing with the library
crate; the produced binary is `leek-dap`.)

## Usage

An editor launches it as a stdio subprocess. With the
[`editors/vscode/`](../../editors/vscode/) extension installed, hit F5 on a
`.leek` file (the extension fills in a default launch config), or write one:

```jsonc
{
  "type": "leek",
  "request": "launch",
  "name": "Debug Leekscript file",
  "program": "${file}",
  "stopOnEntry": false
}
```

Launch options: `version` (language version 1–4, defaults to the file's
pragma), `strict`, and `noDebug` (run natively without breakpoints).

## Debugging an AI inside a fight

Set `scenario` to a fight scenario file (TOML or the official generator's
JSON) and the program is debugged *inside the fight* — breakpoints fire
during the turn loop:

```jsonc
{
  "type": "leek",
  "request": "launch",
  "name": "Debug AI in fight",
  "program": "${workspaceFolder}/ais/hero.leek",
  "scenario": "${workspaceFolder}/duel.toml",
  "stopOnEntry": true
}
```

Fight-specific options: `fightEntity` (entity id the program controls;
defaults to the entity whose `ai` is this program), `profile` (a
`[profiles.<name>]` block applied to the scenario), `seed`, and `maxTurns`.
A runnable example with ready-made launch configs lives in
[`examples/fight/`](../../examples/fight/).
