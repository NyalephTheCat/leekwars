#!/usr/bin/env bash
# Repo-wide quality gate: formatting, lints, build, and tests.
#
# Usage:
#   tools/check.sh          # fast gate: fmt + clippy + tests (skips leek-test-corpus)
#   tools/check.sh --full   # also runs leek-test-corpus (upstream_suite — takes >10 min)
#
# Notes:
#   * `cargo clippy --workspace --all-targets` must be completely quiet — the
#     workspace denies warnings here via `-D warnings`. (The leek-test-corpus
#     build script prints an informational `cargo:warning` about extracted
#     upstream cases; that is not a lint and is tolerated.)
#   * Tests never rewrite tracked snapshot reports; they compare against them
#     and write fresh copies under target/. Set UPDATE_SNAPSHOTS=1 to accept
#     changed reports.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

FULL=0
[[ "${1:-}" == "--full" ]] && FULL=1

step() { printf '\n==> %s\n' "$*"; }

step "cargo fmt --all --check"
cargo fmt --all --check

step "layer check (cargo xtask check-layers)"
cargo xtask check-layers

# Generated weapon/chip catalogs must match the upstream JSON (skipped when
# the official-generator submodule isn't checked out).
if [[ -d official-generator/leek-wars-generator/data ]]; then
  step "weapon/chip catalog drift (tools/game-item-extract.sh --check)"
  tools/game-item-extract.sh --check
fi

step "cargo clippy --workspace --all-targets (-D warnings)"
cargo clippy --workspace --all-targets --quiet -- -D warnings

if (( FULL )); then
  step "cargo test --workspace (full, incl. leek-test-corpus upstream_suite — slow)"
  cargo test --workspace --quiet
else
  step "cargo test --workspace (excluding leek-test-corpus; use --full to include)"
  cargo test --workspace --exclude leek-test-corpus --quiet
fi

step "all checks passed"
