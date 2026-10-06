#!/bin/bash
# Per-minute host-side samples for every VM on the sandbox workers, plus the
# sandbox-manager's own view (`space-sbxmgr list`, HP column) and a copy of
# every worker pod's /space/logs.
#   monitor.sh OUT_DIR [INTERVAL_SECS]
out=${1:?out dir}; every=${2:-60}
here=$(cd "$(dirname "$0")" && pwd)
export PATH=$HOME/.local/bin:$PATH
mkdir -p "$out"
while :; do
  for n in space-local-worker space-local-worker2; do
    docker exec -i "$n" python3 - "$n" < "$here/node_probe.py" >> "$out/vm-samples.jsonl" 2>>"$out/monitor.err"
  done
  for p in $(kubectl --context kind-space-local -n space get pods -l app.kubernetes.io/name=space-worker -o name 2>/dev/null); do
    { echo "## $(date -u +%FT%TZ) ${p#pod/}"; kubectl --context kind-space-local -n space exec "${p#pod/}" -c node-stack -- sh -c 'COLUMNS=400 space-sbxmgr list' 2>&1; } >> "$out/sbxmgr-list.txt"
  done
  "$here/logsnap.sh" "$out/logs" 2>>"$out/monitor.err"
  sleep "$every"
done
