#!/usr/bin/env bash
# Repo-wide quality gate: formatting, lints, build, and tests.
#
# Usage:
#   tools/check.sh          # fast gate: fmt + pin/layer/artifact checks + clippy + tests
#                           (skips leek-test-corpus)
#   tools/check.sh --full   # also runs leek-test-corpus (upstream_suite — takes >10 min)
#
# Notes:
#   * `cargo clippy --workspace --all-targets` must be completely quiet — the
#     workspace denies warnings here via `-D warnings`. (The leek-test-corpus
#     build script prints an informational `cargo:warning` about extracted
#     upstream cases; that is not a lint and is tolerated.)
#   * leek-backend-java's parity tests write their run reports (OPS_DRIFT.txt,
#     JVM_PARITY.txt, CORPUS_SUMMARY.txt, NATIVE_OPS_DRIFT.txt) under
#     target/ — several recorded programs use randInt, so op counts drift
#     run-to-run and the reports are not reproducible. Set UPDATE_SNAPSHOTS=1
#     to refresh the tracked copies in tests/snapshots/ on purpose; the gate
#     asserts afterwards that a plain test run left no snapshot churn behind.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

FULL=0
[[ "${1:-}" == "--full" ]] && FULL=1

step() { printf '\n==> %s\n' "$*"; }

step "cargo fmt --all --check"
cargo fmt --all --check

step "toolchain pin check (cargo xtask check-toolchain)"
cargo xtask check-toolchain

step "layer check (cargo xtask check-layers)"
cargo xtask check-layers

step "generated-artifact check (cargo xtask check-artifacts)"
cargo xtask check-artifacts

# Generated weapon/chip catalogs must match the upstream JSON (skipped when
# the official-generator submodule isn't checked out).
if [[ -d official-generator/leek-wars-generator/data ]]; then
  step "weapon/chip catalog drift (tools/game-item-extract.sh --check)"
  tools/game-item-extract.sh --check
fi

step "cargo clippy --workspace --all-targets (-D warnings)"
cargo clippy --workspace --all-targets --quiet -- -D warnings

# Fingerprint the tracked, non-reproducible run reports so we can prove the
# test run left them alone. Hashing (rather than `git diff`) keeps the check
# honest when the working tree already carries a deliberate report refresh.
# The `.diff`/SUMMARY.txt snapshots next to them are reproducible tracking
# artifacts that a real emitter change is *meant* to update, so they stay out
# of this guard.
SNAPSHOTS="crates/backends/leek-backend-java/tests/snapshots"
REPORTS=(
  "$SNAPSHOTS/OPS_DRIFT.txt"
  "$SNAPSHOTS/JVM_PARITY.txt"
  "$SNAPSHOTS/CORPUS_SUMMARY.txt"
  "$SNAPSHOTS/NATIVE_OPS_DRIFT.txt"
)
reports_fingerprint() {
  sha256sum "${REPORTS[@]}" 2>/dev/null | sha256sum
}
REPORTS_BEFORE="$(reports_fingerprint)"

if (( FULL )); then
  step "cargo test --workspace (full, incl. leek-test-corpus upstream_suite — slow)"
  cargo test --workspace --quiet
else
  step "cargo test --workspace (excluding leek-test-corpus; use --full to include)"
  cargo test --workspace --exclude leek-test-corpus --quiet
fi

# Running the suite must be side-effect free: the java-backend run reports go
# to target/ unless UPDATE_SNAPSHOTS=1 asked for a refresh. Guard against a
# regression that starts writing into the tracked copies again.
case "${UPDATE_SNAPSHOTS:-}" in
  ""|0)
    step "run-report side-effect check ($SNAPSHOTS)"
    if [[ "$(reports_fingerprint)" != "$REPORTS_BEFORE" ]]; then
      echo "error: the test run rewrote the tracked run reports:" >&2
      printf '  %s\n' "${REPORTS[@]}" >&2
      echo "They are not reproducible (op counts drift with randInt); tests" >&2
      echo "must write them under target/ and refresh the tracked copies only" >&2
      echo "when UPDATE_SNAPSHOTS=1 is set." >&2
      exit 1
    fi
    ;;
  *)
    step "UPDATE_SNAPSHOTS set — tracked run reports were refreshed on purpose"
    ;;
esac

step "all checks passed"
