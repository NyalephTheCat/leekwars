#!/usr/bin/env python3
"""Generate a typed, documented Leekscript `.leek` signature header for a
Leek Wars library.

Pipeline (sources, all keyed by function name):
  - <java>          FightFunctions.java / LeekFunctions.java — the `Type.*`
                    signatures + overloads (one per `CallableVersion`).
  - --functions     leek-wars `functions.ts` — real parameter names,
                    `optional`/`deprecated` flags, return name.
  - --docs          leek-wars `doc.<lang>.lang` — prose for the function,
                    each argument, and the return value.
  - --overrides     a TOML file (see tools/library_overrides.toml) letting us
                    replace or drop generated signatures by hand — e.g. to add
                    generics (`push<T>(Array<T> array, T value)`).

Each function becomes a Doxygen-style docstring (`@param`, `@return`,
`@deprecated`) plus one bodiless signature per overload, with real
parameter names.
"""

import argparse
import os
import re
import sys

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    tomllib = None

TYPE_MAP = {
    "INT": "integer",
    "REAL": "real",
    "BOOL": "boolean",
    "STRING": "string",
    "ANY": "any",
    "VOID": "void",
    "NULL": "null",
    "FUNCTION": "Function",
    "ARRAY": "Array",
    "MAP": "Map",
    "INT_OR_NULL": "integer?",
    "BOOL_OR_NULL": "boolean?",
    "STRING_OR_NULL": "string?",
    "ARRAY_OR_NULL": "Array?",
    "ARRAY_INT": "Array<integer>",
    "ARRAY_INT_OR_NULL": "Array<integer>",
    "MAP_INT_STRING": "Map<integer, string>",
    "MAP_STRING_STRING": "Map<string, string>",
    "INT_OR_BOOL": "any",
    "INT_OR_REAL": "any",
}


def map_type(java: str) -> str:
    java = java.strip()
    if java.startswith("new FunctionType"):
        return "Function"
    return TYPE_MAP.get(java.removeprefix("Type."), "any")


# ── Java type source ────────────────────────────────────────────────────

def extract_method_calls(src: str):
    i = 0
    while (m := src.find("method(", i)) >= 0:
        depth, j = 0, m + len("method")
        start = j
        while j < len(src):
            c = src[j]
            if c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
                if depth == 0:
                    yield src[start + 1 : j]
                    break
            j += 1
        i = j + 1


def split_types(s: str):
    out, depth, cur = [], 0, ""
    for ch in s:
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        if ch == "," and depth == 0:
            if cur.strip():
                out.append(cur.strip())
            cur = ""
        else:
            cur += ch
    if cur.strip():
        out.append(cur.strip())
    return out


def parse_callable(body: str):
    ret = (re.match(r"\s*(Type\.[A-Z_]+)", body) or [None, "Type.ANY"])[1] \
        if re.match(r"\s*(Type\.[A-Z_]+)", body) else "Type.ANY"
    pm = re.search(r"new Type\[\]\s*\{([^}]*)\}", body)
    return (ret, split_types(pm.group(1)) if pm else [])


def parse_versions(rest: str):
    versions = [parse_callable(vm.group(1))
                for vm in re.finditer(r"new CallableVersion\(([^;]*?)\)\s*(?:,|$|\])", rest)]
    if versions:
        return versions
    # Inline form: `Type.RET` optionally followed by an args array, which
    # may be `new Type[]{…}` OR a sized empty array `new Type[0]` (0 args).
    inline = re.search(
        r"(Type\.[A-Z_]+)\s*(?:,\s*new Type\[[^\]]*\]\s*(?:\{([^}]*)\})?)?\s*$",
        rest.strip())
    return [(inline.group(1), split_types(inline.group(2) or ""))] if inline else []


def parse_java(path: str):
    """name → (dispatch_class, list of (ret_java_type, [param_java_types]))."""
    src = open(path).read()
    out = {}
    for args in extract_method_calls(src):
        nm = re.match(
            r'\s*"([^"]+)"\s*,\s*"([^"]*)"\s*,\s*\d+\s*,\s*(?:(?:true|false)\s*,\s*)?(.*)',
            args, re.S)
        if not nm:
            continue
        v = parse_versions(nm.group(3))
        if v:
            out[nm.group(1)] = (nm.group(2), v)
    return out


# ── leek-wars functions.ts (names / optional / deprecated) ──────────────

def parse_functions_ts(path: str):
    if not path:
        return {}
    src = open(path, encoding="utf-8").read()
    out = {}
    for obj in re.findall(r"\{[^{}]*\}", src):
        nm = re.search(r"name:\s*'([^']*)'", obj)
        if not nm:
            continue
        names = re.findall(r"'([^']*)'", re.search(r"arguments_names:\s*\[([^\]]*)\]", obj).group(1)) \
            if re.search(r"arguments_names:\s*\[([^\]]*)\]", obj) else []
        opt = re.search(r"optional:\s*\[([^\]]*)\]", obj)
        optional = [t.strip() == "true" for t in opt.group(1).split(",")] if opt and opt.group(1).strip() else []
        out[nm.group(1)] = {
            "names": names,
            "optional": optional,
            "deprecated": bool(re.search(r"deprecated:\s*true", obj)),
            "return_name": (re.search(r"return_name:\s*'([^']*)'", obj) or [None, ""])[1]
            if re.search(r"return_name:\s*'([^']*)'", obj) else "",
        }
    return out


# ── leek-wars doc.<lang>.lang (prose) ───────────────────────────────────

def parse_docs(path: str):
    docs = {}
    if not path:
        return docs
    for line in open(path, encoding="utf-8"):
        m = re.match(r'\s*"(func_[A-Za-z0-9_]+)"\s*:\s*"(.*)"\s*,?\s*$', line)
        if m:
            docs[m.group(1)] = m.group(2)
    return docs


def clean_doc(text: str):
    """HTML → plain text; `<br/>` becomes paragraph breaks."""
    text = re.sub(r"<br\s*/?>", "\n", text)
    text = re.sub(r"<[^>]+>", "", text)
    text = text.replace('\\"', '"').replace("#", "")
    return [re.sub(r"[ \t]+", " ", ln).strip() for ln in text.split("\n") if ln.strip()]


def load_overrides(path: str):
    if not path or not tomllib:
        return {}
    with open(path, "rb") as f:
        data = tomllib.load(f)
    return data.get("functions", {})


# ── emit ────────────────────────────────────────────────────────────────

def doxygen(name, params, ret, fts, docs, dispatch=None):
    """Build the Doxygen comment lines for one function (shared by overloads).

    When `dispatch` is set (game-function library), a hidden
    `@java-dispatch: <DispatchClass>` directive is embedded so the Java
    backend emits `<DispatchClass>.<name>(ai, coerced-args)`.
    """
    desc = docs.get(f"func_{name}")
    lines = ["/**"]
    if desc:
        for ln in clean_doc(desc):
            lines.append(f" * {ln}")
    for i, pname in enumerate(params):
        argdoc = docs.get(f"func_{name}_arg_{i + 1}")
        opt = (fts.get("optional") or [])
        suffix = " (optional)" if i < len(opt) and opt[i] else ""
        txt = " ".join(clean_doc(argdoc)) if argdoc else ""
        lines.append(f" * @param {pname}{(' ' + txt) if txt else ''}{suffix}".rstrip())
    if ret != "void":
        rdoc = docs.get(f"func_{name}_return")
        txt = " ".join(clean_doc(rdoc)) if rdoc else ""
        lines.append(f" * @return{(' ' + txt) if txt else ''}".rstrip())
    if fts.get("deprecated"):
        lines.append(" * @deprecated")
    if dispatch:
        lines.append(f" * @java-dispatch: {dispatch}")
    lines.append(" */")
    # A docstring with only the open/close markers adds no value.
    return lines if len(lines) > 2 else []


# Leekscript reserved words — a parameter named one of these (e.g.
# `default`) is mangled with a trailing `_` so the signature parses.
KEYWORDS = frozenset(
    "var global return function if else while for do in break continue null "
    "true false and or not include class extends this super new static private "
    "public protected constructor instanceof is as xor switch case default "
    "abstract await import export goto catch finally try throw throws typeof "
    "void interface let native package byte char float double int long short "
    "transient volatile synchronized enum eval final with yield implements "
    "const boolean".split()
)


def param_name(fts, i):
    names = fts.get("names") or []
    name = names[i] if i < len(names) and names[i] else f"a{i}"
    return f"{name}_" if name in KEYWORDS else name


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("java")
    ap.add_argument("--functions")
    ap.add_argument("--docs")
    ap.add_argument("--overrides")
    ap.add_argument(
        "--dispatch",
        action="store_true",
        help="emit @java-dispatch directives (game/environment library — the "
        "functions don't exist as language builtins, so the Java backend "
        "must dispatch them to <Class>Class.<name>(ai, …)).",
    )
    args = ap.parse_args()

    java = parse_java(args.java)
    fts_all = parse_functions_ts(args.functions)
    docs = parse_docs(args.docs)
    overrides = load_overrides(args.overrides)

    out = [
        "// @experimental: function_signatures",
        "//",
        f"// GENERATED from {os.path.basename(args.java)} (+ functions.ts, doc.en.lang)",
        "// by tools/gen_leekwars_header.py. Edit tools/library_overrides.toml to",
        "// tweak signatures (e.g. add generics); do not edit this file by hand.",
        "",
    ]
    count = 0
    for name, (clazz, versions) in java.items():
        ov = overrides.get(name, {})
        if ov.get("skip"):
            continue
        fts = fts_all.get(name, {})
        # Doc shared across overloads — use the longest param list for @param.
        widest = max(versions, key=lambda v: len(v[1]))[1]
        params_named = [param_name(fts, i) for i in range(len(widest))]
        # Fully-qualified so the emitted Java needs no import.
        dispatch = f"com.leekwars.generator.classes.{clazz}Class" if (args.dispatch and clazz) else None
        out += doxygen(name, params_named, map_type(versions[0][0]), fts, docs, dispatch)

        if ov.get("signatures"):  # full manual override (e.g. generics)
            for sig in ov["signatures"]:
                out.append(f"function {sig};" if not sig.rstrip().endswith(";") else f"function {sig}")
                count += 1
        else:
            for ret, ptypes in versions:
                ps = ", ".join(f"{map_type(t)} {param_name(fts, i)}" for i, t in enumerate(ptypes))
                out.append(f"function {name}({ps}) -> {map_type(ret)};")
                count += 1
        out.append("")

    sys.stdout.write("\n".join(out))
    sys.stderr.write(f"generated {count} signatures for {len(java)} functions\n")


if __name__ == "__main__":
    main()
