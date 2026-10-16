#!/usr/bin/env bash
# collect-store-logs.sh <dir>
# Follows every space-object-store pod's log into <dir>/<pod>.log until
# killed. The fault scenarios scale the store to 0, which deletes its pod and
# the log with it, so each new pod is followed as soon as it appears.
set -uo pipefail
dir="${1:?output dir}"
ctx="kind-${SPACE_KIND_CLUSTER:-space-local}"
mkdir -p "$dir"
declare -A followed
trap 'kill $(jobs -p) 2>/dev/null; exit 0' TERM INT
while :; do
  for pod in $(kubectl --context "$ctx" -n space get pods -l app.kubernetes.io/name=space-object-store \
      --field-selector=status.phase=Running -o name 2>/dev/null); do
    p="${pod#pod/}"
    if [[ -z "${followed[$p]:-}" ]] || ! kill -0 "${followed[$p]}" 2>/dev/null; then
      kubectl --context "$ctx" -n space logs -f "$p" --all-containers --timestamps --since=1h >>"$dir/$p.log" 2>/dev/null &
      followed[$p]=$!
    fi
  done
  sleep 5
done
