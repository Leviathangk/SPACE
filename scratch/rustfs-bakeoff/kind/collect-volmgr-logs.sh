#!/usr/bin/env bash
# collect-volmgr-logs.sh <dir> [interval-seconds]
# Appends every volume-manager pod's recent log lines to <dir>/<pod>.log every
# interval (default 300 s) until killed, so a suite's transfers survive
# container log rotation and pod restarts. transfer-durations.py dedupes.
set -uo pipefail
dir="${1:?output dir}"; interval="${2:-300}"
ctx="kind-${SPACE_KIND_CLUSTER:-space-local}"
mkdir -p "$dir"
while :; do
  for pod in $(kubectl --context "$ctx" -n space get pods -l space.pplx.net/node-service=volume-manager -o name 2>/dev/null); do
    p="${pod#pod/}"
    kubectl --context "$ctx" -n space logs "$p" --all-containers --timestamps --since="$((interval * 2 + 60))s" >>"$dir/$p.log" 2>/dev/null
    kubectl --context "$ctx" -n space logs "$p" --all-containers --timestamps --previous >>"$dir/$p.previous.log" 2>/dev/null
  done
  sleep "$interval"
done
