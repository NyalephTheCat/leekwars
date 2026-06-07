#!/usr/bin/env bash
# Extract static Java builtin dispatch metadata from upstream *Class.java.
#
# Output: TSV rows `name<TAB>class<TAB>kind` where `kind` is `long` when any
# overload's first non-AI parameter is `long`/`int` (drives emit coercion),
# otherwise `double`.
#
# Usage:
#   tools/builtin-extract.sh              # print to stdout
#   tools/builtin-extract.sh --check      # exit 1 if builtins.tsv drifts
#   tools/builtin-extract.sh --write      # overwrite leek-builtins/builtins.tsv

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CLASSES="$ROOT/official-generator/leek-wars-generator/leekscript/src/main/java/leekscript/runner/classes"
OUT="$ROOT/crates/core/leek-builtins/builtins.tsv"

extract() {
  python3 - "$CLASSES" <<'PY'
import re
import sys
from collections import defaultdict
from pathlib import Path

classes = Path(sys.argv[1])
groups: dict[tuple[str, str], set[str]] = defaultdict(set)

for path in sorted(classes.glob("*Class.java")):
    cls = path.stem
    text = path.read_text()
    for m in re.finditer(r"public static (\S+) (\w+)\(([^)]*)\)", text):
        params = m.group(3)
        if "AI" not in params.split(",")[0]:
            continue
        name = m.group(2)
        parts = [p.strip() for p in params.split(",") if p.strip()]
        key = (name, cls)
        if len(parts) > 1:
            groups[key].add(parts[1].split()[0])
        else:
            groups[key]  # AI-only overload; kind defaults to double

rows = []
for (name, cls), first_params in sorted(groups.items()):
    kind = "long" if any(t in ("long", "int") for t in first_params) else "double"
    rows.append(f"{name}\t{cls}\t{kind}")

print("\n".join(rows))
PY
}

mode="${1:---stdout}"
case "$mode" in
  --stdout)
    extract
    ;;
  --check)
    tmp="$(mktemp)"
    trap 'rm -f "$tmp"' EXIT
    extract >"$tmp"
    if ! diff -u "$OUT" "$tmp"; then
      echo "builtin-extract: $OUT is out of date; run tools/builtin-extract.sh --write" >&2
      exit 1
    fi
    ;;
  --write)
    extract >"$OUT"
    echo "wrote $OUT"
    ;;
  *)
    echo "usage: $0 [--stdout|--check|--write]" >&2
    exit 2
    ;;
esac
