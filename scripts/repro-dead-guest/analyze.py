#!/usr/bin/env python3
"""Summarize one sandbox's auto-suspend attempts from sandbox-manager JSON logs.

  analyze.py SANDBOX_ID LOG [LOG...] [--timeline]

Reads the node-stack `sbxmgr.log` JSON lines and reports, per sandbox:
suspend_requested / suspend_failed (operation, reason) / successful suspend
(`local_artifact_published` with artifact_origin=suspend, or `suspend_completed`),
the "auto lifecycle transition failed" warnings, the budget-exhausted park, and
composed-health changes.
"""

import argparse
import json
from datetime import datetime


def ts(value):
    return datetime.fromisoformat(value.replace("Z", "+00:00"))


def load(sandbox, paths):
    needle = sandbox.removeprefix("sbx_")
    rows = []
    for path in paths:
        with open(path, errors="replace") as handle:
            for line in handle:
                if needle not in line:
                    continue
                try:
                    rows.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
    rows.sort(key=lambda row: row.get("timestamp", ""))
    return rows


def classify(row):
    message = row.get("message", "")
    event = row.get("event_type")
    if message == "space sandbox event":
        if event == "suspend_requested":
            return "suspend_requested", row.get("trigger") or ""
        if event == "suspend_failed":
            return "suspend_failed", f"{row.get('operation')}/{row.get('reason')}"
        if event == "suspend_completed":
            return "suspend_completed", row.get("trigger") or ""
        if event == "local_artifact_published" and "suspend" in json.dumps(row):
            return "suspend_artifact_published", ""
        if event in {"wake_requested", "wake_completed", "restore_requested", "restore_completed"}:
            return event, row.get("trigger") or ""
        return None
    if message.startswith("auto-suspend retry budget exhausted"):
        return "PARKED", ""
    if message == "sandbox composed health changed":
        return "health", f"{row.get('previous')}->{row.get('current')}"
    if message == "auto lifecycle transition failed" and "stage" in row:
        return "transition_failed", (row.get("error") or "")[:140]
    return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("sandbox")
    parser.add_argument("logs", nargs="+")
    parser.add_argument("--timeline", action="store_true")
    args = parser.parse_args()
    rows = load(args.sandbox, args.logs)
    timeline = [(row["timestamp"], *kind) for row in rows if (kind := classify(row))]
    if args.timeline:
        for stamp, kind, detail in timeline:
            print(f"{stamp}  {kind:28s} {detail}")
    requested = [t for t, k, _ in timeline if k == "suspend_requested"]
    failed = [(t, d) for t, k, d in timeline if k == "suspend_failed"]
    done = [t for t, k, _ in timeline if k in {"suspend_completed", "suspend_artifact_published"}]
    parked = [t for t, k, _ in timeline if k == "PARKED"]
    health = [(t, d) for t, k, d in timeline if k == "health"]
    reasons = {}
    for _, detail in failed:
        reasons[detail] = reasons.get(detail, 0) + 1
    first = requested[0] if requested else None
    summary = {
        "sandbox": args.sandbox,
        "attempts": len(requested),
        "failures": len(failed),
        "failure_reasons": reasons,
        "first_attempt": first,
        "parked_at": parked,
        "suspended_at": done[0] if done else None,
        "time_to_suspend_s": round((ts(done[0]) - ts(first)).total_seconds()) if done and first else None,
        "attempts_before_suspend": sum(1 for t in requested if not done or t <= done[0]),
        "health_changes": health,
    }
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
