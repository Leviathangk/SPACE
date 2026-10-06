#!/bin/bash
# watch-exp1.sh RUN_DIR MAX_SECS — return when an incident opens, a workload
# process dies or exits, or MAX_SECS pass; print a one-screen status.
run=${1:?}; max=${2:-1500}; t0=$(date +%s)
seen=$(cat "$run"/*.cycles.jsonl 2>/dev/null | grep -c incident_opened)
while :; do
  now=$(cat "$run"/*.cycles.jsonl 2>/dev/null | grep -c incident_opened)
  alive=$(pgrep -f '[a]si_workload.py run' | wc -l)
  [ "$now" -gt "$seen" ] && { echo "NEW INCIDENT"; break; }
  [ "$alive" -lt "${EXPECT:-12}" ] && { echo "workload processes alive: $alive"; break; }
  avail=$(docker exec space-local-worker3 df -k --output=avail /var | tail -1 | tr -d ' ')
  [ "$avail" -lt 2000000 ] && { echo "DISK LOW: /var avail ${avail} kB"; break; }
  [ $(( $(date +%s) - t0 )) -ge "$max" ] && { echo "checkpoint"; break; }
  sleep 20
done
date -u +%FT%TZ
grep -h -E '"step": "(restore|incident_opened|incident_recovered|incident_unrecovered|cycle_aborted|plan_done|suspend)"' "$run"/*.cycles.jsonl | tail -8 | cut -c1-260
grep -h Traceback -A3 "$run"/*.stdout | tail -8
