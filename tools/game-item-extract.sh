#!/usr/bin/env bash
# Vendor the official generator's item data into the fight engine.
#
# The engine reads the *upstream files themselves* — `data/weapons.json`,
# `data/chips.json`, `data/summons.json` — rather than a transcription of
# them into Rust tables (`crates/game/leek-game-runtime/src/catalog.rs` parses
# them on first use). So this script copies, it does not generate: a generator
# bump shows up as a diff of upstream's own data, and no field can go missing
# because nobody taught a transcriber about it.
#
# The copies are tracked rather than read out of the submodule at build time
# so the crate still builds in a tree without `official-generator` checked
# out; `--check` is what keeps them honest, and it skips (rather than fails)
# when the submodule is absent.
#
# Usage:
#   tools/game-item-extract.sh           # show what a refresh would change
#   tools/game-item-extract.sh --check   # exit 1 if the copies have drifted
#   tools/game-item-extract.sh --write   # refresh the copies from the submodule
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$ROOT/official-generator/leek-wars-generator/data"
DEST="$ROOT/crates/game/leek-game-runtime/data"
FILES=(weapons.json chips.json summons.json)

require_submodule() {
  if [[ ! -d "$SRC" ]]; then
    echo "official-generator submodule is not checked out — cannot reach $SRC" >&2
    echo "run: git submodule update --init --recursive" >&2
    exit 1
  fi
}

case "${1:-}" in
  --write)
    require_submodule
    mkdir -p "$DEST"
    for f in "${FILES[@]}"; do
      cp "$SRC/$f" "$DEST/$f"
      echo "vendored $f ($(wc -c < "$DEST/$f") bytes)"
    done
    ;;
  --check)
    if [[ ! -d "$SRC" ]]; then
      echo "official-generator submodule absent — skipping the item-data check"
      exit 0
    fi
    status=0
    for f in "${FILES[@]}"; do
      if ! diff -q "$SRC/$f" "$DEST/$f" >/dev/null 2>&1; then
        echo "item data drifted: $f" >&2
        status=1
      fi
    done
    if (( status )); then
      echo "run tools/game-item-extract.sh --write to re-vendor" >&2
      exit 1
    fi
    echo "item data up to date"
    ;;
  *)
    require_submodule
    for f in "${FILES[@]}"; do
      diff -u "$DEST/$f" "$SRC/$f" || true
    done
    ;;
esac
