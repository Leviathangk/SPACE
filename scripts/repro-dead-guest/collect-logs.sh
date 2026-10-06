#!/bin/bash
# Follow every sandbox-node node-stack pod's sandbox-manager log into
# $OUT/<pod>.sbxmgr.log (restarts the follow if the pod or exec drops).
OUT=${1:-$HOME/repro-logs/sbxmgr}
mkdir -p "$OUT"
ctx=(--context kind-space-local -n space)
follow() {
  local pod=$1
  while :; do
    kubectl "${ctx[@]}" exec "$pod" -c node-stack -- tail -n +1 -F /space/logs/sbxmgr.log \
      > "$OUT/$pod.sbxmgr.log.tmp" 2>>"$OUT/collector.err"
    # tail -n +1 replays the whole file; keep the longest copy.
    if [ ! -f "$OUT/$pod.sbxmgr.log" ] || [ "$(wc -c <"$OUT/$pod.sbxmgr.log.tmp")" -ge "$(wc -c <"$OUT/$pod.sbxmgr.log")" ]; then
      mv "$OUT/$pod.sbxmgr.log.tmp" "$OUT/$pod.sbxmgr.log"
    fi
    echo "$(date -u +%FT%TZ) follow $pod ended; retrying" >>"$OUT/collector.err"
    sleep 5
  done
}
for pod in $(kubectl "${ctx[@]}" get pods -l app.kubernetes.io/name=space-worker -o name 2>/dev/null | sed 's#pod/##'); do
  follow "$pod" &
done
wait
