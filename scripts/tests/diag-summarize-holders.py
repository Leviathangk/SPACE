#!/usr/bin/env python3
"""DIAGNOSTIC ONLY: summarize diag-sample-mount-holders.py JSONL."""
import json, sys
ns_seen, holders = {}, {}
for line in open(sys.argv[1], errors="replace"):
    try:
        tick = json.loads(line)
    except ValueError:
        continue
    t = tick.get("wall_time_ns", 0)
    for r in tick.get("records", []):
        if r["type"] == "mountinfo":
            for l in r["lines"]:
                f = l.split()
                opt = f[6:f.index("-")] if "-" in f else []
                key = (r["mnt_ns"], f[3], f[4], " ".join(opt))
                e = ns_seen.setdefault(key, [t, t, r["pid"], r["comm"]])
                e[1] = t
        else:
            key = (r["mnt_ns"], r["comm"], r["type"], r.get("target", "")[:160])
            e = holders.setdefault(key, [t, t, r["pid"]])
            e[1] = t
print("=== mount copies by namespace (ns, mountpoint, propagation): first,last,pid,comm")
for k, v in sorted(ns_seen.items(), key=lambda kv: kv[1][0]):
    print(k, v[0], v[1], v[2], v[3], f"dur_ms={(v[1]-v[0])//1_000_000}")
print("=== holders (ns, comm, type, target): first,last,pid")
for k, v in sorted(holders.items(), key=lambda kv: kv[1][0]):
    print(k, v[0], v[1], v[2], f"dur_ms={(v[1]-v[0])//1_000_000}")
