#!/usr/bin/env python3
"""Smoke-test the leek-dap fight debug target over stdio.

Drives: initialize -> setBreakpoints -> launch(scenario, stopOnEntry) ->
configurationDone, expects a `stopped` (entry) event during the fight, then
`continue` and expects the fight to terminate with a winner.
"""
import json
import subprocess
import sys
import threading
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DAP = os.path.join(ROOT, "target/debug/leek-dap")
PROGRAM = os.path.join(ROOT, "examples/fight/ais/hero.leek")
SCENARIO = os.path.join(ROOT, "examples/fight/duel.toml")

proc = subprocess.Popen([DAP], stdin=subprocess.PIPE, stdout=subprocess.PIPE)

seq = 0
def send(msg):
    global seq
    seq += 1
    msg["seq"] = seq
    body = json.dumps(msg).encode()
    proc.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
    proc.stdin.flush()

events = []
done = threading.Event()

def reader():
    buf = b""
    while True:
        chunk = proc.stdout.read(1)
        if not chunk:
            break
        buf += chunk
        if buf.endswith(b"\r\n\r\n"):
            length = int(buf.decode().split("Content-Length:")[1].split("\r")[0].strip())
            payload = proc.stdout.read(length)
            buf = b""
            msg = json.loads(payload)
            kind = msg.get("type")
            if kind == "event":
                ev = msg.get("event")
                events.append((ev, msg.get("body")))
                print(f"<< event {ev}: {json.dumps(msg.get('body'))[:120]}")
                if ev == "stopped":
                    # Resume to let the fight finish.
                    send({"type": "request", "command": "continue", "arguments": {"threadId": 1}})
                if ev == "terminated":
                    done.set()
            elif kind == "response":
                print(f"<< resp {msg.get('command')} success={msg.get('success')}")

threading.Thread(target=reader, daemon=True).start()

send({"type": "request", "command": "initialize", "arguments": {"adapterID": "leek"}})
# Breakpoint on line 4 of hero.leek (the first statement inside the AI).
send({"type": "request", "command": "setBreakpoints",
      "arguments": {"source": {"path": PROGRAM}, "breakpoints": [{"line": 4}]}})
send({"type": "request", "command": "launch",
      "arguments": {"program": PROGRAM, "scenario": SCENARIO, "fightEntity": 1,
                    "stopOnEntry": True}})
send({"type": "request", "command": "configurationDone"})

if not done.wait(timeout=30):
    print("TIMEOUT waiting for terminated event")
    proc.kill()
    sys.exit(2)

got_stop = any(e == "stopped" for e, _ in events)
fight_output = "".join(b.get("output", "") for e, b in events if e == "output" and b)
print("---")
print("stopped during fight:", got_stop)
print("fight output:", fight_output.strip())
ok = got_stop and "fight over" in fight_output
print("RESULT:", "PASS" if ok else "FAIL")
proc.kill()
sys.exit(0 if ok else 1)
