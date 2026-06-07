#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

declare -A LAYER=(
  [leek-span]=core [leek-diagnostics]=core [leek-manifest]=core [leek-builtins]=core
  [leek-syntax]=frontend [leek-lexer]=frontend [leek-parser]=frontend
  [leek-resolver]=middle [leek-types]=middle [leek-hir]=middle [leek-mir]=middle
  [leek-backend-interp]=backends [leek-charge]=middle [leek-complexity]=middle
  [leek-pipeline]=db [leek-backend-java]=backends [leek-runtime]=middle
  [leek-lsp]=tools [leek-fmt]=tools [leek-lint]=tools [leek-migrate]=tools [leek-rewrite]=tools
  [leek-test-corpus]=testing [leek-bench]=testing
)

rank() {
  case "$1" in
    core) echo 0 ;; frontend) echo 1 ;; middle) echo 2 ;; db) echo 3 ;;
    runtime) echo 4 ;; backends|tools|testing) echo 5 ;; *) echo 99 ;;
  esac
}

deps_in() {
  awk '/^\[dependencies\]/ {d=1;next} /^\[/ {d=0} d && /^leek-/ {gsub(/ .*/,"",$1); print $1}' "$1"
}

fail() { echo "layer check: $*" >&2; exit 1; }

while IFS= read -r -d '' m; do
  crate=$(grep '^name' "$m" | head -1 | sed 's/name *= *"\(.*\)"/\1/')
  from=${LAYER[$crate]:-}; [[ -z "$from" ]] && continue
  fr=$(rank "$from")
  while IFS= read -r dep; do
    [[ -z "$dep" ]] && continue
    [[ "$dep" == "leek-pipeline" ]] && continue
    to=${LAYER[$dep]:-}; [[ -z "$to" ]] && continue
    tr=$(rank "$to")
    (( tr > fr )) && fail "$crate ($from) -> $dep ($to)"
  done < <(deps_in "$m")
done < <(find crates bins -name Cargo.toml -print0)

while IFS= read -r dep; do
  to=${LAYER[$dep]:-}
  [[ -n "$to" && "$to" != "core" ]] && fail "leek-pipeline must not depend on $dep ($to)"
done < <(deps_in crates/db/leek-pipeline/Cargo.toml)

echo "layer check: ok"
