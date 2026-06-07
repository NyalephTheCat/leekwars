#!/usr/bin/env bash
# Extract game/combat builtin metadata from the official leek-wars-generator
# `FightFunctions.java` registrations (the ~201 fight functions: getCell,
# moveToward, useWeapon, …). These are the environment functions the
# generator injects on top of the language builtins via
# `LeekFunctions.setExtraFunctions(FightFunctions.getFunctions(),
#  "com.leekwars.generator.classes.*")`.
#
# Output: TSV rows `name<TAB>clazz<TAB>is_static<TAB>min_arity<TAB>max_arity<TAB>ops`
# where `clazz` is the dispatch class (Entity → EntityClass, …).
#
# Usage:
#   tools/game-builtin-extract.sh           # print to stdout
#   tools/game-builtin-extract.sh --check   # exit 1 if game_builtins.tsv drifts
#   tools/game-builtin-extract.sh --write    # overwrite the committed TSV

set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GENSRC="$ROOT/official-generator/leek-wars-generator/src/main/java"
SRC="$GENSRC/com/leekwars/generator/FightFunctions.java"
CONSTSRC="$GENSRC/com/leekwars/generator/FightConstants.java"
OUT="$ROOT/crates/core/leek-environment/game_builtins.tsv"
CONSTOUT="$ROOT/crates/core/leek-environment/game_constants.tsv"

extract() {
  python3 - "$SRC" <<'PY'
import re
import sys

text = open(sys.argv[1]).read()


def balanced(s, start):
    depth = 0
    for i in range(start, len(s)):
        if s[i] == '(':
            depth += 1
        elif s[i] == ')':
            depth -= 1
            if depth == 0:
                return i
    return -1


def split_top(s):
    out, depth, cur = [], 0, ''
    for c in s:
        if c in '([{':
            depth += 1
        elif c in ')]}':
            depth -= 1
        if c == ',' and depth == 0:
            out.append(cur)
            cur = ''
        else:
            cur += c
    if cur.strip():
        out.append(cur)
    return [x.strip() for x in out]


def arg_kind(t):
    # The generator's dispatch methods type INT args as Java `long`, REAL as
    # `double`, BOOL as `boolean`, STRING as `String`; everything else
    # (arrays, *_OR_NULL, OBJECT, ANY, …) as `Object`. The Java backend
    # coerces its Object-typed args to match.
    t = t.replace('Type.', '').strip()
    return {'INT': 'long', 'REAL': 'double', 'BOOL': 'bool', 'STRING': 'string'}.get(t, 'obj')


def kinds_of(type_list_src):
    tm = re.search(r'\{([^}]*)\}', type_list_src)
    if not tm:
        return []
    return [arg_kind(a) for a in tm.group(1).split(',') if a.strip()]


rows = []
for m in re.finditer(r'\bmethod\(', text):
    op = m.end() - 1
    close = balanced(text, op)
    args = split_top(text[op + 1:close])
    # Skip the `method(...)` helper *definitions* (their first arg is a
    # typed parameter like `String name`, not a "string" literal).
    if not args or not args[0].startswith('"'):
        continue
    name = args[0].strip('"')
    clazz = args[1].strip().strip('"')
    idx = 2
    ops = '0'
    if idx < len(args) and re.fullmatch(r'\d+', args[idx]):
        ops = args[idx]
        idx += 1
    is_static = 'true'
    if idx < len(args) and args[idx] in ('true', 'false'):
        is_static = args[idx]
        idx += 1
    rest = ', '.join(args[idx:])
    # Collect the argument-kind list per overload; the widest (max-arity)
    # overload is a prefix-superset, so its per-position kinds cover every
    # call arity.
    versions = []
    for cv in re.finditer(r'new CallableVersion\(', rest):
        s = cv.end() - 1
        cvargs = split_top(rest[s + 1:balanced(rest, s)])
        versions.append(kinds_of(cvargs[1]) if len(cvargs) >= 2 else [])
    if not versions:
        versions = [kinds_of(rest)]
    arities = [len(v) for v in versions]
    widest = max(versions, key=len)
    rows.append((name, clazz, is_static, min(arities), max(arities), ops, ','.join(widest)))

rows.sort()
for name, clazz, is_static, lo, hi, ops, kinds in rows:
    print(f"{name}\t{clazz}\t{is_static}\t{lo}\t{hi}\t{ops}\t{kinds}")
PY
}

# Extract `NAME(value, Type.X)` enum entries from FightConstants.java into
# `name<TAB>kind<TAB>value` rows. `kind` is a coarse type for hover; `value`
# is the resolved integer (empty when not a foldable integer). The value is
# either a literal int or a `Class.FIELD` reference resolved against the
# `static final int FIELD = N` declarations across the generator sources —
# this lets the opt-in constant-folding pass replace e.g. `WEAPON_PISTOL`
# with `37` for every backend.
extract_constants() {
  python3 - "$CONSTSRC" "$GENSRC" <<'PY'
import os
import re
import sys

text = open(sys.argv[1]).read()
gensrc = sys.argv[2]
TYPE = {
    "INT": "integer", "REAL": "real", "BOOL": "boolean", "STRING": "string",
    "VOID": "null", "NULL": "null", "ANY": "any",
}


def kind(t):
    t = t.replace("Type.", "")
    if t.startswith("ARRAY"):
        return "array"
    if t.startswith("MAP"):
        return "map"
    return TYPE.get(t, "any")


# Build a `SimpleClass.FIELD -> initializer` table from every
# `static final int|long|double FIELD = <expr>;` across the generator
# sources, where <expr> is a numeric literal OR another `Class.FIELD`
# reference (alias chains like `Fight.MAX_TURNS = State.MAX_TURNS`).
field_init = {}
for dirpath, _dirs, files in os.walk(gensrc):
    for fn in files:
        if not fn.endswith(".java"):
            continue
        src = open(os.path.join(dirpath, fn), encoding="utf-8").read()
        tm = re.search(r'\b(?:class|enum|interface)\s+([A-Za-z_][A-Za-z0-9_]*)', src)
        if not tm:
            continue
        cls = tm.group(1)
        # `static`/`final` in either order, optional access modifier; the
        # initializer is a numeric literal or a single `Class.FIELD`.
        for fm in re.finditer(
                r'\b(?:public|private|protected)?\s*'
                r'(?:(?:static|final)\s+){2}(?:int|long|double|float)\s+'
                r'([A-Za-z_][A-Za-z0-9_]*)\s*=\s*'
                r'(-?\d+\.?\d*|[A-Za-z_][A-Za-z0-9_.]*)\s*;', src):
            field_init[f"{cls}.{fm.group(1)}"] = fm.group(2).strip()


def resolve(expr, seen=None):
    expr = expr.strip()
    if re.fullmatch(r'-?\d+', expr):
        return str(int(expr))
    if re.fullmatch(r'-?\d+\.\d+', expr):
        return expr  # real literal, kept verbatim
    # A `Class.FIELD` reference — resolve transitively (guard cycles).
    seen = seen or set()
    if expr in seen or expr not in field_init:
        return None
    seen.add(expr)
    return resolve(field_init[expr], seen)


rows = []
# Enum entries: `NAME(value, Type.X)` — value may itself contain parens
# (e.g. `Fight.MAX_TURNS`), so capture the value up to the LAST `, Type.`.
for m in re.finditer(r'^\s*([A-Z][A-Z0-9_]+)\s*\((.*),\s*(Type\.[A-Z_]+)\s*\)', text, re.M):
    name, val_expr, ty = m.group(1), m.group(2), m.group(3)
    val = resolve(val_expr)
    rows.append((name, kind(ty), '' if val is None else val))
rows.sort()
for name, ty, val in rows:
    print(f"{name}\t{ty}\t{val}")
PY
}

# NOTE: the game *functions* are no longer extracted to a TSV — they live
# in the typed signature header `crates/core/leek-prelude/src/leekwars.leek`
# (generated by tools/gen_leekwars_header.py --dispatch), which carries the
# `@java-dispatch:` directives the Java backend uses. Only the fight
# *constants* still come from a table here.
case "${1:-}" in
  --write)
    mkdir -p "$(dirname "$CONSTOUT")"
    extract_constants > "$CONSTOUT"
    echo "wrote $(wc -l < "$CONSTOUT") constants to $CONSTOUT"
    ;;
  --check)
    if ! diff -q <(extract_constants) "$CONSTOUT" >/dev/null 2>&1; then
      echo "game_constants.tsv is out of date — run tools/game-builtin-extract.sh --write" >&2
      exit 1
    fi
    echo "game constants up to date"
    ;;
  *)
    extract_constants
    ;;
esac
