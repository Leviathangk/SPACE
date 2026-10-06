#!/usr/bin/env python3
"""Cross-check suspend attempts against the guest's STOP/CONT log.

  cont-overlap.py SANDBOX_ID STOPLOOP_LOG SBXMGR_LOG [SBXMGR_LOG...]

For each auto-suspend attempt (suspend_requested .. suspend_failed/success),
report whether a CONT window started inside it. The flush budget is 180 s, so
an attempt can only succeed if the agent is continued within that window.
Also prints the empirical CONT coverage: the fraction of attempts whose window
contained a CONT, and the fraction that actually succeeded.
"""

import json
import re
import subprocess
import sys
from datetime import datetime, timedelta
from pathlib import Path


def ts(value):
    value = value.replace("Z", "+00:00")
    # The guest log carries nanoseconds; fromisoformat accepts at most 6 digits.
    value = re.sub(r"(\.\d{6})\d+", r"\1", value)
    return datetime.fromisoformat(value)


sandbox, guest_log, *logs = sys.argv[1:]
conts = [ts(line.split()[0]) for line in Path(guest_log).read_text().splitlines()
         if re.match(r"^\d{4}-\S+Z (CONT|exit CONT)", line)]
stops = [line for line in Path(guest_log).read_text().splitlines() if " STOP " in line]
here = Path(__file__).resolve().parent
timeline = subprocess.run([sys.executable, str(here / "analyze.py"), sandbox, *logs, "--timeline"],
                          capture_output=True, text=True).stdout.splitlines()
events = []
for line in timeline:
    parts = line.split()
    if len(parts) >= 2 and parts[1] in {"suspend_requested", "suspend_failed", "suspend_artifact_published",
                                         "suspend_completed", "PARKED"}:
        events.append((ts(parts[0]), parts[1]))

attempts, current = [], None
for stamp, kind in events:
    if kind == "suspend_requested":
        current = stamp
    elif kind in {"suspend_failed", "suspend_artifact_published", "suspend_completed"} and current:
        attempts.append((current, stamp, kind != "suspend_failed"))
        current = None
if current:
    attempts.append((current, None, None))

print(f"{sandbox}: {len(stops)} STOP periods, {len(conts)} CONT")
covered = 0
for index, (start, end, ok) in enumerate(attempts, 1):
    window_end = end or start + timedelta(seconds=180)
    inside = [c for c in conts if start - timedelta(seconds=2) <= c <= window_end]
    covered += bool(inside)
    took = f"{(end - start).total_seconds():6.1f}s" if end else "   open"
    print(f"  attempt {index:2d} {start:%H:%M:%S} {took} "
          f"{'SUCCESS' if ok else 'failed ' if ok is False else 'pending'} "
          f"cont_inside={[f'{c:%H:%M:%S}' for c in inside]}")
done = [a for a in attempts if a[2] is not None]
if done:
    print(f"  attempts={len(done)} with_CONT={covered} succeeded={sum(1 for a in done if a[2])} "
          f"success_rate={sum(1 for a in done if a[2]) / len(done):.3f}")
print(json.dumps({"stop_periods": stops[:3] + (["..."] if len(stops) > 3 else [])}))
