#!/bin/bash
# run-stress.sh RUN_DIR NAME PHASE CYCLE BUSY_SECS MEM_MB -- [stress.py burst args]
# One stress variant: stress.py burst, then resume every sandbox's workload for
# BUSY_SECS as phase PHASE (memhog held at MEM_MB).
run=${1:?}; name=${2:?}; phase=${3:?}; cycle=${4:?}; busy=${5:?}; mem=${6:?}; shift 6; [ "${1:-}" = "--" ] && shift
here=$(cd "$(dirname "$0")" && pwd)
python3 "$here/driver.py" login
python3 "$here/stress.py" burst "$run" --name "$name" "$@" > "$run/stress-$name.stdout" 2>&1 || exit 1
for f in "$run"/a*.json; do
  label=$(basename "$f" .json)
  setsid nohup python3 "$here/agent_workload.py" run "$label" --out "$run" --attach --attach-phase "$phase" \
    --cycle "$cycle" --plan "" --warm "$busy" --mem-start-mb "$mem" --mem-final-mb "$mem" --ramp 1 \
    </dev/null >> "$run/$label.stdout" 2>&1 &
done
wait
