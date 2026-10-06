#!/usr/bin/env python3
"""Per-minute `space.sandbox.guest.health` for lifecycle started and pausing
(an in-flight suspend attempt holds the sandbox in `pausing`), summed over the
captured nodes.

  gauge-series.py FROM_ISO TO_ISO STATSD_FILE [STATSD_FILE...] [--every N]
"""
import re
import sys
from collections import defaultdict

args = [a for a in sys.argv[1:] if not a.startswith("--every")]
every = int(next((a.split("=")[1] for a in sys.argv[1:] if a.startswith("--every=")), "5"))
start, end, *files = args
pattern = re.compile(r"^(\S+)Z space\.sandbox\.guest\.health:([\d.]+)\|g\|.*lifecycle:(started|pausing),health:(\w+)")
last = defaultdict(dict)  # minute -> (file, health) -> value
for path in files:
    for line in open(path):
        match = pattern.match(line)
        if not match or not (start <= match[1] <= end):
            continue
        last[match[1][:16]][(path, match[3] + '/' + match[4])] = float(match[2])
print("minute(UTC)        started/healthy started/degraded pausing/healthy pausing/degraded")
for index, minute in enumerate(sorted(last)):
    if index % every:
        continue
    totals = defaultdict(float)
    for (_, health), value in last[minute].items():
        totals[health] += value
    print(f"{minute}   {totals['started/healthy']:15.0f} {totals['started/degraded']:16.0f} "
          f"{totals['pausing/healthy']:15.0f} {totals['pausing/degraded']:16.0f}")
