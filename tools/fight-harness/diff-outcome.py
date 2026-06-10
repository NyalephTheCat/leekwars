#!/usr/bin/env python3
"""Diff two fight Outcome JSONs (candidate vs golden).

Used by the conformance suite: the Rust simulator replays a golden's
scenario+seed and its Outcome must match the official generator's byte for
byte, modulo volatile fields (wall-clock `execution_time`) and — until op
accounting reaches parity (see OPS_DRIFT.txt) — the `ops` tables when
--ignore-ops is given.

The action log gets first-class treatment: actions are compact arrays
`[type_id, args...]`, so the first divergent index is reported with both
sides decoded, which usually names the subsystem at fault (pathfinding,
crit roll, effect formula, ...).

Usage: diff-outcome.py <candidate.json> <golden.json> [--ignore-ops]
Exit:  0 identical (modulo ignores), 1 divergent, 2 usage/parse error.
"""

import json
import sys

ACTION_NAMES = {
    0: "StartFight", 5: "EntityDie", 6: "NewTurn", 7: "EntityTurn",
    8: "EndTurn", 9: "Invocation", 10: "Move", 11: "Kill", 12: "UseChip",
    13: "SetWeapon", 14: "StackEffect", 15: "ChestOpened", 16: "UseWeapon",
    101: "Damage", 103: "Heal", 104: "Vitality", 105: "Resurrect",
    107: "NovaDamage", 108: "DamageReturn", 109: "LifeDamage", 110: "Poison",
    112: "NovaVitality", 201: "Lama", 203: "Say", 205: "ShowCell",
    301: "AddWeaponEffect", 302: "AddChipEffect", 303: "RemoveEffect",
    304: "UpdateEffect", 306: "ReduceEffects", 307: "RemovePoisons",
    308: "RemoveShackles", 1002: "AiError",
}


def decode(action):
    if isinstance(action, list) and action and isinstance(action[0], int):
        name = ACTION_NAMES.get(action[0], f"?{action[0]}")
        return f"{name}{action[1:]}"
    return repr(action)


def diff_actions(cand, gold):
    """Report the first divergent action; returns the number of differences."""
    n = 0
    for i, (c, g) in enumerate(zip(cand, gold)):
        if c != g:
            print(f"actions[{i}] differ:")
            print(f"  candidate: {decode(c)}")
            print(f"  golden:    {decode(g)}")
            n += 1
            break  # everything after the first divergence is noise
    if len(cand) != len(gold):
        print(f"action count: candidate {len(cand)} vs golden {len(gold)}")
        if not n:
            longer, who = (cand, "candidate") if len(cand) > len(gold) else (gold, "golden")
            i = min(len(cand), len(gold))
            print(f"  first extra ({who}): actions[{i}] = {decode(longer[i])}")
        n += 1
    return n


def walk(cand, gold, path, ignored, out):
    if any(path.endswith(ig) for ig in ignored):
        return
    if type(cand) is not type(gold):
        out.append(f"{path}: type {type(cand).__name__} vs {type(gold).__name__}")
    elif isinstance(cand, dict):
        for k in sorted(set(cand) | set(gold)):
            if k not in cand:
                out.append(f"{path}.{k}: missing in candidate")
            elif k not in gold:
                out.append(f"{path}.{k}: not in golden")
            else:
                walk(cand[k], gold[k], f"{path}.{k}", ignored, out)
    elif isinstance(cand, list):
        if len(cand) != len(gold):
            out.append(f"{path}: length {len(cand)} vs {len(gold)}")
        for i, (c, g) in enumerate(zip(cand, gold)):
            walk(c, g, f"{path}[{i}]", ignored, out)
    elif cand != gold:
        out.append(f"{path}: {cand!r} vs {gold!r}")


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    flags = {a for a in sys.argv[1:] if a.startswith("--")}
    if len(args) != 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    try:
        cand = json.load(open(args[0]))
        gold = json.load(open(args[1]))
    except (OSError, json.JSONDecodeError) as e:
        print(f"error: {e}", file=sys.stderr)
        return 2

    ignored = [".execution_time"]
    if "--ignore-ops" in flags:
        ignored += [".ops", ".fight.ops"]

    differences = 0

    # Actions first: the most diagnostic divergence.
    cand_actions = cand.get("fight", {}).get("actions", [])
    gold_actions = gold.get("fight", {}).get("actions", [])
    differences += diff_actions(cand_actions, gold_actions)

    # Everything else (excluding actions, already handled).
    rest = []
    walk(
        {k: v for k, v in cand.items() if k != "fight"} |
        {"fight": {k: v for k, v in cand.get("fight", {}).items() if k != "actions"}},
        {k: v for k, v in gold.items() if k != "fight"} |
        {"fight": {k: v for k, v in gold.get("fight", {}).items() if k != "actions"}},
        "", ignored, rest,
    )
    for line in rest[:40]:
        print(line)
    if len(rest) > 40:
        print(f"... and {len(rest) - 40} more differences")
    differences += len(rest)

    if differences:
        print(f"DIVERGENT ({differences} difference(s))")
        return 1
    print("identical (modulo ignored fields)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
