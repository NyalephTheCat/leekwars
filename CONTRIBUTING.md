# Contributing to Leekscript

Thanks for your interest in hacking on the Rust LeekScript toolchain! This
document covers how to set up the repo, the quality bar your changes need to
clear, and the conventions we follow. For a tour of *what* lives where, read
[`docs/architecture.md`](docs/architecture.md); for the language itself, see
[`docs/grammar.md`](docs/grammar.md) and the top-level [`README.md`](README.md).

By participating you agree to abide by our
[Code of Conduct](CODE_OF_CONDUCT.md).

## Prerequisites

- **Rust** — the toolchain is pinned to an *exact* release in
  [`rust-toolchain.toml`](rust-toolchain.toml) (1.94.1, with `rustfmt` and
  `clippy`). With `rustup` installed it is selected automatically; no manual
  `rustup install` needed. That pin is also the workspace MSRV — see
  [Bumping the Rust toolchain](#bumping-the-rust-toolchain).
- **Git submodules** — the upstream reference implementations
  (`official/`, `official-generator/`) are vendored as submodules. They power
  the corpus tests and the generated weapon/chip catalog drift check.
- **A JDK** *(optional)* — only needed for the Java-backend parity tests and
  the `rust-java` benchmark; everything else builds and tests without it.
- **Node 22** *(optional)* — only to work on the VS Code extension under
  [`editors/vscode/`](editors/vscode/).
- **Python 3** *(optional)* — used by the catalog/builtin extraction scripts
  in [`tools/`](tools/).

## Getting set up

```sh
git clone --recursive <repo-url> leekscript
cd leekscript
cargo build            # builds the whole workspace
cargo test             # runs the tests (see the gate below for the full story)
```

If you cloned without `--recursive`, fetch the submodules:

```sh
git submodule update --init --recursive
```

Install the binaries on your `PATH` while iterating (optional):

```sh
cargo install --path bins/miku    # likewise leekc, leek-lsp, leek-dap, leekbench
```

## The quality gate

[`tools/check.sh`](tools/check.sh) is the canonical, repo-wide gate, and it is
exactly what CI runs. **Run it before opening a pull request:**

```sh
tools/check.sh          # fmt + pin/layer/artifact checks + catalog drift + clippy + tests
tools/check.sh --full   # also runs the slow upstream corpus suite (>10 min)
```

It is the source of truth, so the individual steps below are just for when you
want to run one in isolation:

```sh
cargo fmt --all                          # format (--check to verify only)
cargo xtask check-toolchain              # Rust pin and MSRV agree, and are exact
cargo xtask check-layers                 # enforce the crate-layering rule
cargo xtask check-artifacts              # generated output stays untracked
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --exclude leek-test-corpus   # fast tests
```

CI mirrors this across `fmt`, `clippy`, `test`, plus a `cargo-deny`
supply-chain audit and a VS Code extension build — see
[`.github/workflows/ci.yml`](.github/workflows/ci.yml).

## Code style & standards

- **Formatting** is non-negotiable: `cargo fmt` must leave no diff. There is no
  custom `rustfmt.toml` — the default stable style is the style.
- **Clippy** runs at `pedantic` (warn) and CI treats every warning as an error
  (`-D warnings`). A small set of judgment-heavy pedantic lints is allowed
  workspace-wide in [`Cargo.toml`](Cargo.toml); prefer fixing the code over
  adding new `#[allow]`s, and if you must allow something locally, scope it
  tightly and say why.
- **No `unsafe`** — `unsafe_code` is denied across the workspace.
- **No leftover-debugging footguns** — `dbg!`, `todo!`, and `unimplemented!`
  are lint-flagged. Don't ship them in real code paths.
- **`missing_errors_doc` / `missing_panics_doc` are allowed**, but a short
  doc comment on public items is still appreciated.

### The layering rule

Crates are organized into layers, and dependencies may only point *down* the
stack. `cargo xtask check-layers` enforces this and CI fails if it's violated:

```
core → frontend → middle → db → backends → game → tools · testing → bins · xtask
```

A crate's layer is its directory. Peers (`·`) may not depend on each other,
dev-dependencies may reach one layer up, and `leek-pipeline` must depend only
on `core`. Known exceptions live in
[`xtask/layer-allowlist.txt`](xtask/layer-allowlist.txt); that list should only
shrink. If you find yourself wanting an "upward" dependency, that's usually a
sign the abstraction belongs in a lower layer — see
[`docs/architecture.md`](docs/architecture.md).

### Bumping the Rust toolchain

[`rust-toolchain.toml`](rust-toolchain.toml) pins an **exact** release, not
`stable`. Clippy runs at `pedantic` with `-D warnings`, so a floating channel
lets any new Rust release turn CI red with no commit in this repo — that is
what commit fb6e336 ("Fix CI pipeline failures on current toolchains") had to
clean up, seven files edited only to satisfy newer lints.

The pin is also the MSRV: `[workspace.package] rust-version` in
[`Cargo.toml`](Cargo.toml) names the pin's `MAJOR.MINOR`, which is what makes
it a real promise — every build in CI uses exactly that compiler, so no extra
job is needed to check it. `cargo xtask check-toolchain` enforces both halves
(exact pin, matching MSRV, `rustfmt`/`clippy` still requested) and CI runs it.

Moving to a newer release is its **own pull request**, so the lint fixes it
drags in never hide inside an unrelated change:

1. bump `channel` in [`rust-toolchain.toml`](rust-toolchain.toml);
2. set `rust-version` in [`Cargo.toml`](Cargo.toml) to the same `MAJOR.MINOR`;
3. run `tools/check.sh` and fix whatever the new clippy release flags.

## Commit messages

Follow the existing history: a lowercase `area: imperative summary` subject,
where `area` is the component you touched (e.g. the crate's short name or a
subsystem):

```
complexity: track size variables for class fields
docs: add formal LeekScript grammar specification
native: unbox scalar locals whose type is known
```

Keep the subject under ~72 characters, and use the body to explain *why* when
the change isn't self-evident. Group related work into focused commits rather
than one sprawling change.

## Pull requests

1. **Branch** off `main` for your work.
2. **Run `tools/check.sh`** and make sure it's green (`--full` if you touched
   anything corpus- or parity-related).
3. **Keep PRs focused.** Smaller, reviewable changes merge faster.
4. **Update docs** alongside behavior changes — the relevant `README.md`,
   `docs/`, or per-binary docs under `bins/*/README.md`.
5. **Open the PR**; CI will run the same gate. Address review feedback by
   pushing follow-up commits to the branch.

## Tests

- Unit and integration tests live next to the code they cover (`tests/` per
  crate). Run a single crate with `cargo test -p <crate>` (e.g.
  `cargo test -p leek-scenario`).
- The **upstream corpus** (`leek-test-corpus`) checks thousands of
  `equals(...)` cases against the reference implementation. It's slow and gated
  behind `tools/check.sh --full`; it needs the submodules checked out.
- **Java-backend snapshots:** nothing under
  `crates/backends/leek-backend-java/tests/snapshots/` is written by a plain
  test run. The per-fixture `.diff` files and `SUMMARY.txt` are goldens and are
  *compared*; the four run reports (`OPS_DRIFT.txt`, `JVM_PARITY.txt`,
  `CORPUS_SUMMARY.txt`, `NATIVE_OPS_DRIFT.txt`) are statistics and go to
  `target/`. Accept new output on purpose with `UPDATE_SNAPSHOTS=1 cargo test
  -p leek-backend-java`, and commit it only when that is the point of the
  change. `tools/check.sh` fails if a run touches the directory without the
  flag.
- **JVM parity gates:** the Java-backend cross-check against the upstream
  harness skips (with a `SKIPPED` line) when `leekscript-emitter.jar` or the
  captured `snapshot.tsv` is missing, so the suite runs without a JDK. Set
  `LEEK_REQUIRE_JVM=1` to turn those skips into failures; `tools/check.sh`
  does it for you when the jar is built. See
  [`docs/java-backend.md`](docs/java-backend.md) §8.
- **Fuzzing:** [`fuzz/`](fuzz/) is a standalone nightly cargo-fuzz workspace
  (excluded from the stable build) — see its README to run a target.

## Adding a new crate

1. Create it under the right layer directory in `crates/` (or `bins/`).
2. Add it to `[workspace].members` and, if other crates depend on it, to
   `[workspace.dependencies]` in [`Cargo.toml`](Cargo.toml).
3. Use `version`, `edition`, and `license` via `workspace = true`, and inherit
   the workspace lints with `[lints] workspace = true`.
4. There is no crate list to update for the layering rule: `cargo xtask
   check-layers` takes the layer from the directory and fails on a crate that
   is not under a layer directory.

## Questions

Open an issue or start a discussion. When reporting a bug, a minimal `.leek`
reproducer and the exact `miku`/`cargo` command you ran go a long way.

## License

By contributing, you agree that your contributions are dual-licensed under
**MIT OR Apache-2.0**, the same terms as the project.
