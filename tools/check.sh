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
#   * leek-backend-java's test runs rewrite OPS_DRIFT.txt and JVM_PARITY.txt
#     (under crates/backends/leek-backend-java/tests/snapshots/)
#     non-deterministically — several recorded programs use randInt, so op
#     counts drift run-to-run. The gate reverts them afterwards so a check
#     run never leaves churn in the working tree.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

FULL=0
[[ "${1:-}" == "--full" ]] && FULL=1

step() { printf '\n==> %s\n' "$*"; }

step "cargo fmt --all --check"
cargo fmt --all --check

step "layer check (tools/check-layers.sh)"
tools/check-layers.sh

step "cargo clippy --workspace --all-targets (-D warnings)"
cargo clippy --workspace --all-targets --quiet -- -D warnings

if (( FULL )); then
  step "cargo test --workspace (full, incl. leek-test-corpus upstream_suite — slow)"
  cargo test --workspace --quiet
else
  step "cargo test --workspace (excluding leek-test-corpus; use --full to include)"
  cargo test --workspace --exclude leek-test-corpus --quiet
fi

# The java-backend tests regenerate these snapshots with non-deterministic
# op counts (randInt); revert them so the gate is side-effect free.
SNAPSHOTS="crates/backends/leek-backend-java/tests/snapshots"
for f in "$SNAPSHOTS/OPS_DRIFT.txt" "$SNAPSHOTS/JVM_PARITY.txt"; do
  if git rev-parse --is-inside-work-tree >/dev/null 2>&1 \
     && ! git diff --quiet -- "$f" 2>/dev/null; then
    step "reverting non-deterministic $f churn"
    git checkout -- "$f"
  fi
done

step "all checks passed"
