#!/bin/bash
# Usage: run-batch.sh BUILD N STOP_MIN STOP_JITTER CONT [IDLE]
# Creates N stalled sandboxes (STOP loop) plus one control without a loop,
# all with an AccessIdle->suspend stage of IDLE (default 90s).
set -u
BUILD=$1 N=$2 STOP_MIN=$3 STOP_JITTER=$4 CONT=$5 IDLE=${6:-90s}
here=$(cd "$(dirname "$0")" && pwd)
runs=$HOME/repro-logs/runs
labels=("$BUILD-control")
for i in $(seq 1 "$N"); do labels+=("$BUILD-stall-$i"); done
for label in "${labels[@]}"; do
  python3 "$here/driver.py" create "$label" --idle "$IDLE" >"$runs/$label.create.log" 2>&1 &
done
wait
for i in $(seq 1 "$N"); do
  label=$BUILD-stall-$i
  sbx=$(python3 -c "import json;print(json.load(open('$runs/$label.json'))['sandbox_id'])")
  python3 "$here/driver.py" stoploop "$sbx" --stop-min "$STOP_MIN" --stop-jitter "$STOP_JITTER" --cont "$CONT" \
    >"$runs/$label.stoploop.log" 2>&1 &
done
wait
for label in "${labels[@]}"; do
  printf '%s %s\n' "$label" "$(python3 -c "import json;print(json.load(open('$runs/$label.json'))['sandbox_id'])")"
done
