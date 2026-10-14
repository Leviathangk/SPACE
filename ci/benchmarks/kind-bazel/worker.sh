#!/usr/bin/env bash
set -euo pipefail
export DOCKER_HOST=unix:///run/space-dind.sock
export SPACE_KIND_DATA_ROOT=/var/lib/space-kind-data
export KIND_CLUSTER=space-local SPACE_KIND_CLUSTER=space-local
export SPACE_KIND_TELEMETRY_MODE=otel
export SPACE_KIND_RUST_ARTIFACTS=prebuilt
export SPACE_KIND_PREBUILT_RUST_SHA
SPACE_KIND_PREBUILT_RUST_SHA="$(git rev-parse HEAD)"
export SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE=/run/space-admin-token
export SPACE_SMOKE_KIND_COLD_RECEIPTS=/run/space-cold-receipts
export API_URL=http://127.0.0.1:8080 DATA_PLANE_URL=http://127.0.0.1:8090
export SPACE_DATA_PLANE_URL="$DATA_PLANE_URL" SPACE_SMOKE_BACKEND=kind
export SPACE_TILT_LOG=/run/space-tilt.log SPACE_TILT_PIDFILE=/run/space-tilt.pid

if [[ "$1" == bootstrap ]]; then
  for tool in docker ip kind kubectl helm tilt go uv curl jq lsof ssh-keygen timeout git tar; do
    command -v "$tool" >/dev/null || { echo "Missing Kind worker prerequisite: $tool" >&2; exit 2; }
  done
  ip link add space-docker0 type bridge
  ip addr add 172.30.0.1/16 dev space-docker0
  ip link set space-docker0 up
  dockerd --host="$DOCKER_HOST" --data-root=/var/lib/docker \
    --exec-root=/run/space-dind --pidfile=/run/space-dind.pid \
    --bridge=space-docker0 --default-address-pool=base=172.31.0.0/16,size=24 \
    --feature containerd-snapshotter=true >/run/space-dockerd.log 2>&1 &
  for _ in $(seq 1 60); do
    if docker info >/dev/null 2>&1; then break; fi
    sleep 1
  done
  docker info >/dev/null
  docker load --input /run/space-artifacts.tar
  export SPACE_KIND_PREBUILT_RUST_IMAGE_ID
  SPACE_KIND_PREBUILT_RUST_IMAGE_ID="$(docker image inspect -f '{{.Id}}' space-rust-build-kind:local)"
  bash infra/space/kind/scripts/prepare-cluster.sh
  bash scripts/tests/smoke-kind-e2e.sh
  # The child reads the exported endpoint.
  # shellcheck disable=SC2016
  timeout 300 sh -c 'until curl -sf -m 2 "$DATA_PLANE_URL/healthz" >/dev/null; do sleep 2; done'
  install -d -m 0700 "$SPACE_SMOKE_KIND_COLD_RECEIPTS"
  curl -fsS -X POST "$API_URL/auth/v1/sessions" \
    -H 'content-type: application/json' \
    -d '{"email":"root@local","password":"password"}' \
    | jq -er '.token | select(type == "string" and length > 0)' >"$SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE"
  chmod 0600 "$SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE"
  # Package installation is done once per stack, outside scenario execution.
  uv sync --frozen --python 3.11
  exit 0
fi

export SPACE_KIND_PREBUILT_RUST_IMAGE_ID
SPACE_KIND_PREBUILT_RUST_IMAGE_ID="$(docker image inspect -f '{{.Id}}' space-rust-build-kind:local)"
export SKIP_BOOTSTRAP=1
source scripts/tests/lib/typed-kind-fixture.sh
prepare_native_kind
ensure_build_foreign_org
export SPACE_SMOKE_API_SURFACE="$2"
exec uv run --frozen --python 3.11 -m space_smoke run "$3"
