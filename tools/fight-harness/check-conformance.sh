#!/usr/bin/env bash
# Conformance check: replay every corpus.txt golden (a MIRROR fight: the same
# AI on both sides) through the RUST simulator (the `official-fight` bin,
# which mirrors Harness.java exactly) and diff each Outcome against the Java
# generator's golden with diff-outcome.py. `ops` / `execution_time` are
# runtime measurements and are ignored; everything else must match
# byte-for-byte. The same check runs as a cargo test:
# crates/game/leek-scenario/tests/conformance.rs.
#
# Usage: check-conformance.sh [name-filter]
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
EX="$HERE/examples"
GOLD="$HERE/goldens"
FILTER="${1:-}"

echo "building official-fight..." >&2
cargo build -q -p leek-scenario --bin official-fight --manifest-path "$ROOT/Cargo.toml" || exit 1
BIN="$ROOT/target/debug/official-fight"

pass=0 fail=0
while read -r name ai seed; do
  [[ -z "$name" || "$name" == \#* ]] && continue
  if [[ -n "$FILTER" && "$name" != *"$FILTER"* ]]; then continue; fi
  golden="$GOLD/$name.json"
  if [[ ! -f "$golden" ]]; then
    echo "SKIP  $name (no golden — run gen-goldens.sh)" >&2
    continue
  fi
  ours=$(mktemp)
  if ! "$BIN" "$EX/$ai" "$EX/$ai" "$seed" > "$ours"; then
    echo "FAIL  $name (official-fight errored)"
    fail=$((fail + 1)); rm -f "$ours"; continue
  fi
  if out=$(python3 "$HERE/diff-outcome.py" "$ours" "$golden" --ignore-ops); then
    echo "PASS  $name"
    pass=$((pass + 1))
  else
    echo "FAIL  $name"
    echo "$out" | sed 's/^/      /'
    fail=$((fail + 1))
  fi
  rm -f "$ours"
done <"$HERE/corpus.txt"

echo "----"
echo "$pass passed, $fail failed"
[[ $fail -eq 0 ]]
