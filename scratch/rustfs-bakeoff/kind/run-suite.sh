#!/usr/bin/env bash
# run-suite.sh <label> — one full Kind run as the bake-off ran it, from the
# repository root, on a box with no Kind cluster:
#
#   1. a fresh bring-up and the baseline smoke (scripts/tests/smoke-kind-e2e.sh)
#      with CI's settings: SPACE_KIND_TELEMETRY_MODE=otel and DATA_PLANE_URL;
#   2. the admin token file and cold-receipts directory, made as
#      .github/workflows/kind-ci.yml makes them;
#   3. the health gate (health-gate.sh);
#   4. the full typed suite, unsharded: SKIP_BOOTSTRAP=1 smoke-kind-typed.sh all;
#   5. throughout, volume-manager's logs (collect-volmgr-logs.sh) and the Docker
#      host's free space; afterwards the transfer durations
#      (transfer-durations.py).
#
# The storage mode comes from the caller's SPACE_KIND_STORAGE_CREDENTIALS
# (unset: static). Output goes to $BAKEOFF_KIND_OUT/<label> (default
# /tmp/rustfs-bakeoff-kind). Leaves the cluster up for inspection.
set -uo pipefail
label="${1:?label}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
out="${BAKEOFF_KIND_OUT:-/tmp/rustfs-bakeoff-kind}/$label"
mkdir -p "$out"
cd "$repo"
export SPACE_KIND_TELEMETRY_MODE=otel
export DATA_PLANE_URL=http://127.0.0.1:8090
export BOOTSTRAP_TIMEOUT="${BOOTSTRAP_TIMEOUT:-3600}"
log() { printf '[run-suite %s] %s %s\n' "$label" "$(date -u +%FT%TZ)" "$*" | tee -a "$out/run.log"; }

disk() { # the Docker host's free space, from the daemon's own view
  docker run --rm --entrypoint df busybox -B1 / 2>/dev/null | awk 'NR == 2 { printf "%.1f GiB free", $4 / 1073741824 }'
}
log "mode=${SPACE_KIND_STORAGE_CREDENTIALS:-static} commit=$(git rev-parse --short HEAD) docker host: $(disk)"
docker system df >>"$out/run.log" 2>&1

(while sleep 600; do log "docker host: $(disk)"; done) &
disk_pid=$!
trap 'kill $disk_pid ${collector_pid:-} 2>/dev/null' EXIT

log "bring-up + baseline smoke"
start=$SECONDS
bash scripts/tests/smoke-kind-e2e.sh >"$out/e2e.log" 2>&1
e2e=$?
log "smoke-kind-e2e.sh exit=$e2e after $((SECONDS - start)) s"
(( e2e == 0 )) || exit "$e2e"

bash "$here/collect-volmgr-logs.sh" "$out/volmgr-logs" 300 &
collector_pid=$!

token_file="$out/admin-token"
receipts="$out/cold-receipts"
install -d -m 0700 "$receipts"
curl -fsS -X POST http://localhost:8080/auth/v1/sessions -H 'content-type: application/json' \
  -d '{"email":"root@local","password":"password"}' | jq -r '.token // empty' >"$token_file"
chmod 0600 "$token_file"
[[ -s "$token_file" ]] || { log "could not obtain an admin session token"; exit 1; }

log "health gate"
bash "$here/health-gate.sh" >>"$out/run.log" 2>&1 || { log "health gate failed"; exit 1; }

log "typed suite (all, unsharded)"
start=$SECONDS
SKIP_BOOTSTRAP=1 SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE="$token_file" SPACE_SMOKE_KIND_COLD_RECEIPTS="$receipts" \
  bash scripts/tests/smoke-kind-typed.sh all >"$out/typed.log" 2>&1
typed=$?
log "smoke-kind-typed.sh all exit=$typed after $((SECONDS - start)) s"

# One last pass so the final transfers are in.
kill "$collector_pid" 2>/dev/null
SPACE_KIND_CLUSTER="${SPACE_KIND_CLUSTER:-space-local}" timeout 120 bash -c "
  for pod in \$(kubectl --context kind-space-local -n space get pods -l app.kubernetes.io/name=space-volume-manager -o name); do
    kubectl --context kind-space-local -n space logs \"\${pod#pod/}\" --all-containers --timestamps >>\"$out/volmgr-logs/\${pod#pod/}.log\" 2>/dev/null
  done"
python3 "$here/transfer-durations.py" "$out/volmgr-logs" >"$out/transfer-durations.md"
log "transfer durations: $(head -n 3 "$out/transfer-durations.md" | tail -n 1)"
log "docker host: $(disk)"
exit "$typed"
