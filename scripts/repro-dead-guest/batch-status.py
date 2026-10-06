#!/usr/bin/env python3
"""One line per sandbox of a batch: attempts, failures, park, suspend, health.

  batch-status.py BUILD LOGDIR
"""
import glob
import json
import subprocess
import sys
from pathlib import Path

build, logdir = sys.argv[1], sys.argv[2]
runs = Path.home() / "repro-logs" / "runs"
here = Path(__file__).resolve().parent
logs = sorted(glob.glob(f"{logdir}/*.log") + glob.glob(f"{logdir}/*.log.tmp"))
for state in sorted(runs.glob(f"{build}-*.json")):
    meta = json.loads(state.read_text())
    out = subprocess.run([sys.executable, str(here / "analyze.py"), meta["sandbox_id"], *logs],
                         capture_output=True, text=True).stdout
    s = json.loads(out)
    health = s["health_changes"][-1][1] if s["health_changes"] else "-"
    print(f"{meta['label']:14s} attempts={s['attempts']:3d} fail={s['failures']:3d} "
          f"reasons={s['failure_reasons']} parked={s['parked_at'][:1]} "
          f"suspended={s['suspended_at']} t_suspend={s['time_to_suspend_s']} "
          f"n_before={s['attempts_before_suspend']} health={health}")
