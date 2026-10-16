#!/usr/bin/env bash
# health-gate.sh — the gate run before each Kind suite in the bake-off:
#   1. Tilt quiet: every resource built and running (or not applicable), with
#      no build pending, queued or in progress, for 60 s in a row;
#   2. no ReplicaSet in the space namespace younger than 180 s;
#   3. every Kubernetes node Ready, and every sandbox and builder node
#      published (infra/space/kind/scripts/wait-node-publication.sh);
#   4. then a 120 s settle.
# Exits nonzero when a step does not hold within its timeout.
set -euo pipefail
ctx="kind-${SPACE_KIND_CLUSTER:-space-local}"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
log() { printf '[health-gate] %s %s\n' "$(date -u +%T)" "$*"; }

tilt_busy() {
  # Prints resources that are building, pending or failed; empty when quiet.
  timeout 20s tilt get uiresources --port 10350 -o json 2>/dev/null | jq -r '
    .items[] | select(
      (.status.updateStatus // "none") as $u | (.status.runtimeStatus // "none") as $r |
      ($u | IN("ok", "not_applicable", "none") | not)
      or ($r | IN("ok", "not_applicable", "none", "unknown") | not)
      or ((.status.pendingBuildSince // "") != "")
      or ((.status.queued // false) == true)
      or ((.status.currentBuild.startTime // "") != "")
    ) | "\(.metadata.name) update=\(.status.updateStatus) runtime=\(.status.runtimeStatus)"'
}

if timeout 5s tilt get session --port 10350 >/dev/null 2>&1; then
  quiet_since=""
  deadline=$((SECONDS + 1800))
  while :; do
    busy="$(tilt_busy || echo "tilt unreachable")"
    if [[ -z "$busy" ]]; then
      quiet_since="${quiet_since:-$SECONDS}"
      (( SECONDS - quiet_since >= 60 )) && break
    else
      quiet_since=""
    fi
    (( SECONDS < deadline )) || { log "Tilt never went quiet:"; printf '%s\n' "$busy"; exit 1; }
    sleep 5
  done
  log "Tilt quiet for 60 s"
else
  log "no Tilt session on port 10350; skipping the Tilt check"
fi

deadline=$((SECONDS + 900))
while :; do
  young="$(kubectl --context "$ctx" -n space get rs -o json | jq -r --argjson now "$(date +%s)" '
    .items[] | select(($now - (.metadata.creationTimestamp | fromdateiso8601)) < 180)
    | .metadata.name')"
  [[ -z "$young" ]] && break
  (( SECONDS < deadline )) || { log "ReplicaSets still younger than 180 s: $young"; exit 1; }
  sleep 10
done
log "no ReplicaSet younger than 180 s"

kubectl --context "$ctx" wait --for=condition=Ready node --all --timeout=600s >/dev/null
log "every node Ready"
bash "$repo/infra/space/kind/scripts/wait-node-publication.sh" 900
log "every node published; settling 120 s"
sleep 120
log "gate passed"
