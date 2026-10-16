#!/usr/bin/env python3
"""Per-scenario verdicts from a smoke-kind-typed.sh log.

Reads the runner's RESULT lines (one JSON object per scenario) under each
"--- typed smoke on the <surface> API surface" header and prints the suite
totals, every scenario that did not pass with its summary, and the verdict of
each scenario named with --show (for the fault scenarios).

Usage: suite-summary.py <typed.log> [--show <scenario_id>]...
"""
import json
import sys
from collections import Counter


def main():
    path = sys.argv[1]
    show = [sys.argv[i + 1] for i, a in enumerate(sys.argv) if a == "--show"]
    surface, results, suites = "?", {}, []
    for line in open(path, errors="replace"):
        if line.startswith("--- typed smoke on the "):
            surface = line.split()[5]
        elif line.startswith("SUITE:") and len(suites) < 2:
            suites.append((surface, line.strip()))
        elif line.startswith("RESULT: "):
            try:
                r = json.loads(line[8:])
            except ValueError:
                continue
            results[(surface, r.get("scenario_id"))] = (r.get("verdict"), r.get("summary", ""))
    for s, line in suites:
        print(f"- {s}: {line}")
    counts = Counter((s, v) for (s, _), (v, _) in results.items())
    print("\n| Surface | Verdict | Scenarios |\n|---|---|---|")
    for (s, v), n in sorted(counts.items()):
        print(f"| {s} | {v} | {n} |")
    bad = [(k, v) for k, v in sorted(results.items()) if v[0] != "pass"]
    print(f"\nNot passed: {len(bad)}")
    for (s, sid), (v, summary) in bad:
        print(f"- {s} `{sid}`: {v}: {summary[:300]}")
    for sid in show:
        hits = [(s, v) for (s, i), (v, _) in results.items() if i == sid]
        print(f"- `{sid}`: " + (", ".join(f"{v} ({s})" for s, v in hits) or "not run"))


if __name__ == "__main__":
    main()
