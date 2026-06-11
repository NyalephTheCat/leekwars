#!/usr/bin/env bash
# Regenerate the conformance goldens: each corpus.txt entry runs one MIRROR
# fight (the same AI on both sides) through the OFFICIAL Java generator (via
# fight.sh) and stores its Outcome JSON under goldens/. The Rust simulator is
# conformance-tested by replaying the same scenario+seed and diffing against
# these files (see diff-outcome.py).
#
# Usage: gen-goldens.sh [name-filter]
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
EX="$HERE/examples"
OUT="$HERE/goldens"
FILTER="${1:-}"
mkdir -p "$OUT"

while read -r name ai seed; do
  [[ -z "$name" || "$name" == \#* ]] && continue
  if [[ -n "$FILTER" && "$name" != *"$FILTER"* ]]; then continue; fi
  echo "=== $name ($ai mirror, seed $seed)" >&2
  # </dev/null: children (gradle/java) must not eat the corpus from stdin
  "$HERE/fight.sh" "$EX/$ai" "$EX/$ai" "$seed" >"$OUT/$name.json" </dev/null
done <"$HERE/corpus.txt"

echo "goldens written to $OUT" >&2
