#!/usr/bin/env bash
# Regenerate the conformance goldens: each entry runs one fight through the
# OFFICIAL Java generator (via fight.sh) and stores its Outcome JSON under
# goldens/. The Rust simulator is conformance-tested by replaying the same
# scenario+seed and diffing against these files (see diff-outcome.py).
#
# Usage: gen-goldens.sh [name-filter]
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
EX="$HERE/examples"
OUT="$HERE/goldens"
FILTER="${1:-}"
mkdir -p "$OUT"

# name  ai1  ai2  seed   — one fight per line.
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

while read -r name ai1 ai2 seed; do
  [[ -z "$name" ]] && continue
  if [[ -n "$FILTER" && "$name" != *"$FILTER"* ]]; then continue; fi
  echo "=== $name ($ai1 vs $ai2, seed $seed)" >&2
  # </dev/null: children (gradle/java) must not eat the corpus from stdin
  "$HERE/fight.sh" "$EX/$ai1" "$EX/$ai2" "$seed" >"$OUT/$name.json" </dev/null
done <<<"$CORPUS"

echo "goldens written to $OUT" >&2
