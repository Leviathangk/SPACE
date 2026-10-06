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
  # Disk headroom: the Kind nodes' /var shares the Docker data disk.
  echo "$(date -u +%FT%TZ) var_avail_kb=$(docker exec space-local-worker3 df -k --output=avail /var | tail -1 | tr -d ' ') home_avail_kb=$(df -k --output=avail /home/dev | tail -1 | tr -d ' ') cache_alloc_kb=$(du -k /home/dev/.kind-data/worker*/containerd/repro-space-cache/space-cache.img 2>/dev/null | awk '{s+=$1} END {print s+0}')" >> "$out/disk.log"
  "$here/logsnap.sh" "$out/logs" 2>>"$out/monitor.err"
  sleep "$every"
done
