# leekc

The Leekscript **single-file compiler driver** — the `rustc`-equivalent. Where
[`miku`](../miku/) drives whole projects, `leekc` takes one `.leek` file and
runs it, checks it, or dumps any intermediate stage of the pipeline. It is the
tool for inspecting what the compiler does and for producing standalone
artifacts.

## Build & install

```sh
# from the repository root
cargo build -p leekc                  # debug build → target/debug/leekc
cargo run -p leekc -- <args>          # run without installing
cargo install --path bins/leekc       # install into ~/.cargo/bin
```

## Usage

```sh
leekc program.leek                       # diagnostics only (default: --emit check)
leekc program.leek --emit run            # JIT-compile (native backend) and run
leekc program.leek --emit fmt            # format and print to stdout
leekc program.leek --emit java -o out/   # emit Java source + .lines sidecar
```

### Inspecting the pipeline

Every intermediate representation can be dumped:

```sh
leekc program.leek --emit tokens     # lexer output
leekc program.leek --emit flat-cst   # flat CST (lexer only, no parser)
leekc program.leek --emit cst        # structured CST
leekc program.leek --emit hir        # resolved + typed tree
leekc program.leek --emit mir        # CFG of basic blocks per function
```

### Native backend (Cranelift)

`--emit native` selects the native backend; `--native-emit` picks the artifact:

```sh
leekc program.leek --emit native                          # JIT-run (default)
leekc program.leek --emit native --native-emit clif       # dump Cranelift IR
leekc program.leek --emit native --native-emit asm        # target disassembly
leekc program.leek --emit native --native-emit object \
      --native-out program.o                              # relocatable object
leekc program.leek --emit native --native-emit exe \
      --native-out program --release                      # standalone executable
```

`--native-emit exe` compiles ahead-of-time to a self-contained binary
(first run = steady-state, no per-run compilation). It needs `cargo` on
`PATH` (it builds the AOT runtime) and a C linker. `--release` switches to
the optimized profile (`--opt-level speed`); `--debug-info` emits DWARF so
a native debugger can map machine code back to source. `--link-game`
compiles leek-wars fight builtins (`getCell`, `useWeapon`, …) as calls into
the game runtime instead of rejecting them.

### Diagnostics & language options

```sh
leekc program.leek --version-pragma 3        # override the file's @version
leekc program.leek --message-format json     # newline-delimited JSON diagnostics
leekc program.leek --deny W0010 --allow E0240
leekc program.leek --library leekwars        # know the fight builtins
```

`--library` accepts a built-in name (`leekwars` for the leek-wars-generator
fight functions) or a path to a library-definition file; its functions become
known to the whole pipeline, and `--emit java` dispatches them to the
library's classes.

### Java backend options

`--clean` emits the readable/optimized variant instead of the byte-faithful
reference shape; `--ai-id <N>` bakes the id into the class name (`AI_<N>`);
`--base-class EntityAI` produces a class the leek-wars-generator can run
directly in a fight; `--fold-constants` folds known library constants
(`WEAPON_PISTOL` → `37`) before emit.

Run `leekc --help` for the full flag reference.
