#!/usr/bin/env python3
"""Per-sandbox Process.Start / raw-vsock timeline for experiment 2.

  e2-summary.py E2_DIR   (reads probe.jsonl, vm-samples.jsonl, triggers.txt)
Prints, per sandbox: first/last probe, OK->fail and fail->OK transitions, the
last error, and the raw CONNECT 5002 result counts over the same span.
"""
import json
import sys
from collections import defaultdict
from pathlib import Path

d = Path(sys.argv[1])
probes = defaultdict(list)
for line in open(d / "probe.jsonl"):
    try:
        p = json.loads(line)
    except ValueError:
        continue
    probes[p["sbx"]].append(p)
vsock = defaultdict(lambda: [0, 0])
for line in open(d / "vm-samples.jsonl"):
    v = json.loads(line)
    vsock["sbx_" + v["sbx"]][0 if v["vsock_ok"] else 1] += 1
print(open(d / "triggers.txt").read())
for sbx, rows in probes.items():
    transitions, prev = [], None
    for row in rows:
        if row["ok"] != prev:
            transitions.append(f"{row['ts'][11:19]}{'+OK' if row['ok'] else '-FAIL'}")
            prev = row["ok"]
    last_err = next((r.get("error", "")[:90] for r in reversed(rows) if not r["ok"]), "")
    ok, bad = vsock.get(sbx, [0, 0])
    print(f"{sbx[-8:]} probes={len(rows)} {rows[0]['ts'][11:19]}..{rows[-1]['ts'][11:19]} "
          f"transitions={' '.join(transitions)} vsock_ok={ok} vsock_fail={bad} last_err={last_err}")
