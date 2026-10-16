#!/usr/bin/env python3
"""Snapshot transfer durations from volume-manager's logs.

Reads the files collect-volmgr-logs.sh wrote, keeps each job runner
"job completed" / "job failed" line once, and prints p50, p95 and max of
elapsed_ms per job kind and subject type (sandbox: snapshot uploads and
downloads; template: template pulls), plus every transfer over the
threshold.

Usage: transfer-durations.py <dir> [--over-ms 5000]
"""
import json, re, sys
from pathlib import Path

def fields(line):
    body = line.split(" ", 1)[1] if line[:4].isdigit() else line
    body = body.strip()
    if body.startswith("{"):
        try:
            v = json.loads(body)
        except ValueError:
            return None
        flat = dict(v.get("fields", {}))
        flat.update({k: x for k, x in v.items() if k != "fields"})
        return flat
    # key=value text format
    out = {k: x.strip('"') for k, x in re.findall(r'(\w+)=("[^"]*"|\S+)', body)}
    m = re.search(r'(job completed|job failed[^ ]*|job execution failed)', body)
    if m:
        out["message"] = m.group(1)
    return out

def pct(vals, p):
    vals = sorted(vals)
    if not vals:
        return None
    k = max(0, min(len(vals) - 1, round(p / 100 * len(vals) + 0.5) - 1))
    return vals[k]

def main():
    d = Path(sys.argv[1])
    over = int(sys.argv[sys.argv.index("--over-ms") + 1]) if "--over-ms" in sys.argv else 5000
    seen, jobs, failures = set(), {}, []
    for f in sorted(d.glob("*.log")):
        for line in f.read_text(errors="replace").splitlines():
            if "job " not in line or line in seen:
                continue
            seen.add(line)
            v = fields(line)
            if not v:
                continue
            msg = str(v.get("message", ""))
            if msg == "job completed" and "elapsed_ms" in v:
                key = (v.get("job_id"), f.name.split(".")[0])
                # subject_id is logged as Some("sandbox:<uuid>") or Some("template:<name>").
                subject = re.sub(r'^Some\("|"\)$', "", str(v.get("subject_id", "")))
                kind = f'{v.get("kind")} ({subject.split(":")[0] or "no subject"})'
                jobs[key] = (kind, int(v["elapsed_ms"]), int(v.get("bytes", 0) or 0), line[:30])
            elif msg.startswith("job") and "fail" in msg:
                failures.append((f.name, v.get("kind"), v.get("job_id"), v.get("error_kind"), line[:30]))
    kinds = sorted({k for k, *_ in jobs.values()})
    print("| Kind | Transfers | p50 (s) | p95 (s) | max (s) | over %.0f s |" % (over / 1000))
    print("|---|---|---|---|---|---|")
    rows = list(jobs.values())
    for kind in kinds + ["all"]:
        vals = [ms for k, ms, *_ in rows if kind in ("all", k)]
        n_over = sum(1 for x in vals if x > over)
        print(f"| {kind} | {len(vals)} | {pct(vals, 50) / 1000:.2f} | {pct(vals, 95) / 1000:.2f} | {max(vals) / 1000:.2f} | {n_over} |")
    slow = sorted((ms, k, b, t) for k, ms, b, t in rows if ms > over)
    if slow:
        print(f"\nTransfers over {over / 1000:.0f} s:\n")
        for ms, k, b, t in slow:
            print(f"- {t.strip()} {k}: {ms / 1000:.2f} s, {b / 1048576:.1f} MiB")
    print(f"\nFailed jobs logged: {len(failures)}")
    for f in failures[:20]:
        print(f"- {f[4].strip()} {f[1]} job {f[2]} ({f[3]})")

if __name__ == "__main__":
    main()
