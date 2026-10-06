#!/usr/bin/env python3
"""Coverage and failure summary for experiment 1 (asi_workload.py runs).

  exp1-summary.py RUN_DIR [MONITOR_DIR]

RUN_DIR holds <label>.ops.jsonl / <label>.cycles.jsonl; MONITOR_DIR (monitor.sh
output) adds per-VM raw vsock results and peak Firecracker RSS.
Busy time is counted per phase from its first to its last operation;
"post-restore" phases are the ones that follow a restore.
"""
import json
import re
import sys
from collections import Counter, defaultdict
from datetime import datetime
from pathlib import Path

run = Path(sys.argv[1])
monitor = Path(sys.argv[2]) if len(sys.argv) > 2 else None


def ts(value):
    return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").timestamp()


def pct(values, q):
    values = sorted(values)
    return values[min(len(values) - 1, int(q * len(values)))] if values else None


phase_span = {}
ops = Counter()
fails = defaultdict(list)
latency = defaultdict(list)
mem_peak = defaultdict(int)
sbx_of = {}
for path in sorted(run.glob("*.ops.jsonl")):
    for line in open(path):
        o = json.loads(line)
        key = (o["label"], o["phase"])
        t = ts(o["ts"])
        lo, hi = phase_span.get(key, (t, t))
        phase_span[key] = (min(lo, t), max(hi, t))
        sbx_of[o["label"]] = o["sbx"]
        ops[(o["op"], o.get("kind"), o["ok"])] += 1
        if o["ok"]:
            latency[o.get("kind")].append(o["ms"])
        else:
            fails[o["label"]].append(o)
        if o.get("kind") == "telemetry" and o.get("out"):
            lines = o["out"].splitlines()
            if len(lines) >= 3 and lines[2].strip().isdigit():
                mem_peak[o["label"]] = max(mem_peak[o["label"]], int(lines[2]))

restores = []
incidents = []
for path in sorted(run.glob("*.cycles.jsonl")):
    for line in open(path):
        c = json.loads(line)
        if c["step"] == "restore":
            restores.append(c)
        if c["step"].startswith("incident"):
            incidents.append(c)

post = sum(hi - lo for (label, phase), (lo, hi) in phase_span.items() if phase.startswith("post-restore"))
warm = sum(hi - lo for (label, phase), (lo, hi) in phase_span.items() if phase == "warm")
print(f"sandboxes: {len(sbx_of)}")
print(f"busy post-restore: {post / 3600:.1f} sandbox-hours; warm (post-create): {warm / 3600:.1f} sandbox-hours")
kinds = Counter((r["kind"], r["result"], r.get("cross_node")) for r in restores)
print("restores (requested kind, result, landed cross-node):", dict(kinds))
total = sum(ops.values())
failed = sum(v for (op, kind, ok), v in ops.items() if not ok)
print(f"operations: {total}, failed: {failed}")
for (op, kind, ok), v in sorted(ops.items(), key=str):
    if not ok:
        print(f"  failed {op}/{kind}: {v}")
print("latency ms (p50/p99/max) by kind:")
for kind, values in sorted(latency.items(), key=lambda kv: str(kv[0])):
    print(f"  {kind}: {pct(values, .5)}/{pct(values, .99)}/{max(values)} n={len(values)}")
print("memhog peak MB per sandbox:", dict(sorted(mem_peak.items())))
print(f"incident lines: {len(incidents)}")
for c in incidents:
    print("  ", json.dumps(c)[:400])
for label, rows in sorted(fails.items()):
    print(f"failures {label} ({sbx_of[label]}): {len(rows)}")
    for o in rows[:8]:
        err = re.sub(r"req_[0-9a-f-]+", "req_…", (o.get("err") or o.get("out") or "")[:160])
        print(f"   {o['ts']} {o['phase']} +{o['since_connect_s']}s {o['op']}/{o.get('kind')} {err}")

if monitor:
    rss = defaultdict(int)
    vs = defaultdict(Counter)
    for line in open(monitor / "vm-samples.jsonl"):
        v = json.loads(line)
        rss[v["sbx"]] = max(rss[v["sbx"]], v["rss_kb"])
        vs[v["sbx"]][v["vsock_ok"]] += 1
    print("per-VM peak Firecracker RSS MB / raw vsock ok:fail")
    for label, sbx in sorted(sbx_of.items()):
        u = sbx.removeprefix("sbx_")
        print(f"  {label} {u[-8:]} rss_peak={rss[u] // 1024} vsock={vs[u][True]}:{vs[u][False]}")
