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
#   * leek-backend-java's parity tests never write into tests/snapshots/ on a
#     plain run: every report there — the per-fixture .diff files, SUMMARY.txt
#     and the four run reports (OPS_DRIFT.txt, JVM_PARITY.txt,
#     CORPUS_SUMMARY.txt, NATIVE_OPS_DRIFT.txt) — is compared against its
#     tracked copy, and the fresh output goes to target/. Op counts are made
#     reproducible by leaving RNG-driven programs out of the exact comparison.
#     Set UPDATE_SNAPSHOTS=1 to accept new output into tests/snapshots/ on
#     purpose; the gate asserts afterwards that a plain test run left no
#     snapshot churn behind. The reports are pinned on Linux (this script's
#     platform); elsewhere they are skipped unless LEEK_REQUIRE_SNAPSHOTS=1.
#   * The JVM parity ratchets only run where tools/java-emitter/build.sh has
#     produced leekscript-emitter.jar. Where it exists this script exports
#     LEEK_REQUIRE_JVM=1 so a broken harness fails instead of skipping.
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

# Fingerprint the whole tracked snapshot directory so we can prove the test
# run left every file in it alone — the reproducible .diff/SUMMARY.txt goldens
# as well as the non-reproducible run reports. Modification times are part of
# the fingerprint on purpose: a rewrite that happens to reproduce the same
# bytes is still a test writing into the source tree, and that is what used to
# hide unreviewed emit drift. Hashing (rather than `git diff`) keeps the check
# honest when the working tree already carries a deliberate refresh.
SNAPSHOTS="crates/backends/leek-backend-java/tests/snapshots"
snapshots_fingerprint() {
  find "$SNAPSHOTS" -type f -printf '%T@ %s %p\n' 2>/dev/null | sort | sha256sum
}
SNAPSHOTS_BEFORE="$(snapshots_fingerprint)"

# The JVM parity ratchets (value / ops / harness errors) are the only check on
# the emitted Java actually running, and they skip when the upstream harness
# jar is missing. Where it has been built, demand it: a harness that fails to
# spawn must fail the gate rather than pass by saying nothing.
if [[ -f tools/java-emitter/build/leekscript-emitter.jar ]]; then
  export LEEK_REQUIRE_JVM=1
  step "upstream emitter jar present — JVM parity gates are required"
fi

if (( FULL )); then
  step "cargo test --workspace (full, incl. leek-test-corpus upstream_suite — slow)"
  cargo test --workspace --quiet
else
  step "cargo test --workspace (excluding leek-test-corpus; use --full to include)"
  cargo test --workspace --exclude leek-test-corpus --quiet
fi

# Running the suite must be side-effect free: the java-backend tests compare
# against tests/snapshots/ and write everything they produce under target/
# unless UPDATE_SNAPSHOTS=1 asked for a refresh. Guard against a regression
# that starts writing into the tracked copies again.
case "${UPDATE_SNAPSHOTS:-}" in
  ""|0)
    step "snapshot side-effect check ($SNAPSHOTS)"
    if [[ "$(snapshots_fingerprint)" != "$SNAPSHOTS_BEFORE" ]]; then
      echo "error: the test run wrote into $SNAPSHOTS" >&2
      echo "Files whose contents also changed (empty if only mtimes moved):" >&2
      git status --porcelain -- "$SNAPSHOTS" >&2 || true
      echo "A plain run must compare against the tracked snapshots and write" >&2
      echo "its own output under target/. Accept new output on purpose with" >&2
      echo "UPDATE_SNAPSHOTS=1, and commit it only when that is the change." >&2
      exit 1
    fi
    ;;
  *)
    step "UPDATE_SNAPSHOTS set — tracked snapshots were refreshed on purpose"
    ;;
esac

step "all checks passed"
