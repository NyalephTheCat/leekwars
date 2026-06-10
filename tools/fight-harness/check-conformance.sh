#!/usr/bin/env bash
# Conformance check: replay every golden's scenario+seed through the RUST
# simulator (the `official-fight` bin, which mirrors Harness.java exactly)
# and diff each Outcome against the Java generator's golden with
# diff-outcome.py. `ops` / `execution_time` are runtime measurements and are
# ignored; everything else must match byte-for-byte.
#
# Usage: check-conformance.sh [name-filter]
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
EX="$HERE/examples"
GOLD="$HERE/goldens"
FILTER="${1:-}"

# Keep this corpus in sync with gen-goldens.sh.
CORPUS=$(cat <<'EOF'
chase_vs_chase_s1      chase.leek   chase.leek   1
chase_vs_chase_s7      chase.leek   chase.leek   7
chase_vs_chase_s42     chase.leek   chase.leek   42
chase_vs_chase_s99     chase.leek   chase.leek   99
chase_vs_chase_s12345  chase.leek   chase.leek   12345
idle_vs_idle_s42       idle.leek    idle.leek    42
walker_vs_walker_s7    walker.leek  walker.leek  7
walker_vs_walker_s42   walker.leek  walker.leek  42
chase_vs_idle_s42      chase.leek   idle.leek    42
chase_vs_walker_s99    chase.leek   walker.leek  99
EOF
)

echo "building official-fight..." >&2
cargo build -q -p leek-scenario --bin official-fight --manifest-path "$ROOT/Cargo.toml" || exit 1
BIN="$ROOT/target/debug/official-fight"

pass=0 fail=0
while read -r name ai1 ai2 seed; do
  [[ -z "$name" ]] && continue
  if [[ -n "$FILTER" && "$name" != *"$FILTER"* ]]; then continue; fi
  golden="$GOLD/$name.json"
  if [[ ! -f "$golden" ]]; then
    echo "SKIP  $name (no golden — run gen-goldens.sh)" >&2
    continue
  fi
  ours=$(mktemp)
  if ! "$BIN" "$EX/$ai1" "$EX/$ai2" "$seed" > "$ours"; then
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
done <<< "$CORPUS"

echo "----"
echo "$pass passed, $fail failed"
[[ $fail -eq 0 ]]
