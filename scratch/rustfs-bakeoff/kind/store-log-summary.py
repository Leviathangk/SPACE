#!/usr/bin/env python3
"""HTTP status counts from the object store's request log.

Reads the files collect-store-logs.sh wrote. RustFS logs one JSON line per
request ("HTTP request completed", with method, uri and status_code); lines
seen twice (a re-followed pod) count once. Prints counts per method and
status, and every 5xx with its time and URI.

Usage: store-log-summary.py <dir>
"""
import json
import sys
from collections import Counter
from pathlib import Path


def main():
    seen, counts, errors = set(), Counter(), []
    for f in sorted(Path(sys.argv[1]).glob("*.log")):
        for line in f.read_text(errors="replace").splitlines():
            body = line.split(" ", 1)[1] if line[:4].isdigit() else line
            if '"status_code"' not in body:
                continue
            try:
                v = json.loads(body)
            except ValueError:
                continue
            if v.get("request_id") in seen:
                continue
            seen.add(v.get("request_id"))
            if v.get("uri", "").startswith("/health"):
                continue
            status = int(v["status_code"])
            counts[(v.get("method"), status)] += 1
            if status >= 500:
                errors.append((v.get("timestamp", "")[:23], v.get("method"), status, v.get("uri", "")[:160]))
    print("| Method | Status | Requests |\n|---|---|---|")
    for (m, s), n in sorted(counts.items(), key=lambda kv: (str(kv[0][0]), kv[0][1])):
        print(f"| {m} | {s} | {n} |")
    print(f"\n5xx responses: {len(errors)}")
    for e in errors[:40]:
        print(f"- {e[0]} {e[1]} {e[2]} {e[3]}")


if __name__ == "__main__":
    main()
