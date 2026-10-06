#!/bin/bash
# run-exp1.sh OUT_DIR [N] — launch N agent_workload.py sandboxes, staggered so
# their suspend/restore steps spread out. Odd labels restore cross,same,cross;
# even labels same,cross,same, so every cycle mixes both restore types.
# PLAN=... (may be empty) overrides the plan for every label.
out=${1:?out dir}; n=${2:-12}; stagger=${STAGGER:-25}
here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out"
for i in $(seq -w 1 "$n"); do
  if [ -n "${PLAN+set}" ]; then plan=$PLAN
  elif (( 10#$i % 2 )); then plan=cross,same,cross; else plan=same,cross,same; fi
  setsid nohup python3 "$here/agent_workload.py" run "a$i" --out "$out" --plan "$plan" \
    --warm "${WARM:-600}" --busy "${BUSY:-3600}" --ramp "${RAMP:-14400}" \
    --memory-mib "${MEMORY_MIB:-2048}" --mem-final-mb "${MEM_FINAL_MB:-1500}" \
    </dev/null > "$out/a$i.stdout" 2>&1 &
  sleep "$stagger"
done
