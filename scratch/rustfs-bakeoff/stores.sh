#!/usr/bin/env bash
# stores.sh — run the two candidate stores side by side on one Docker host,
# with the same settings, for the bake-off probes.
#
#   stores.sh up <rustfs|versitygw>        start (fresh data volume), print start → ready
#   stores.sh restart <rustfs|versitygw>   restart on the same data, print start → ready
#   stores.sh down <rustfs|versitygw>      remove the container and its data volume
#   stores.sh env <rustfs|versitygw>       print BAKEOFF_* exports for the probe
#   stores.sh vend <rustfs>                create the snapshots/templates buckets and the
#                                          vend identity with its user policy (as the Kind chart does)
#   stores.sh veth <store>                 host-side veth of the store's container
#   stores.sh throttle <store> <rate>      cap traffic into the store (tc tbf, e.g. 240mbit)
#   stores.sh unthrottle <store>
#
# Containers are named bakeoff-<store>, data volumes bakeoff-<store>-data, and
# both get the Kind chart's limits (BAKEOFF_LIMITS, default "--cpus 1
# --memory 4g"; set it empty for none). All
# credentials are synthetic local-dev values. Needs docker, curl (with
# --aws-sigv4) and, for throttle, sudo tc on a host whose network namespace
# holds the Docker bridge.
set -euo pipefail

RUSTFS_IMAGE="${RUSTFS_IMAGE:-rustfs/rustfs:1.0.1@sha256:1803faef57627e2d9c2e7d89d655d712ddded5389040054987163043fecb6a3c}"
VERSITYGW_IMAGE="${VERSITYGW_IMAGE:-ghcr.io/versity/versitygw:v1.8.0@sha256:30292fc2eeacc67a36993b01f7a7a5e3361a19cced0e80c1d71cfa2a4b0a2499}"
ADMIN_KEY="${BAKEOFF_ADMIN_KEY:-localadmin}"
ADMIN_SECRET="${BAKEOFF_ADMIN_SECRET:-localadmin-dev-secret}"
VEND_KEY="${BAKEOFF_VEND_KEY:-localvend}"
VEND_SECRET="${BAKEOFF_VEND_SECRET:-localvend-dev-secret}"
REGION=us-east-1
PORT=8333
# The Kind chart's limits for the object store (charts/space-object-store).
read -r -a LIMITS <<<"${BAKEOFF_LIMITS---cpus 1 --memory 4g}"
# Extra server settings for RustFS, e.g. BAKEOFF_RUSTFS_ENV="RUSTFS_X=1 RUSTFS_Y=2".
read -r -a RUSTFS_EXTRA <<<"${BAKEOFF_RUSTFS_ENV:-}"

die() { printf 'stores.sh: %s\n' "$*" >&2; exit 1; }

store="${2:-}"
name="bakeoff-$store"
volume="bakeoff-$store-data"

health_path() {
  case "$store" in
    rustfs) echo /health/ready ;;
    versitygw) echo /healthz ;;
    *) die "unknown store '$store'" ;;
  esac
}

container_ip() {
  docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$name"
}

endpoint() { echo "http://$(container_ip):$PORT"; }

run_container() {
  case "$store" in
    rustfs)
      # Request logging (one line per completed request) is on: the freeze
      # probe reads it. RustFS demotes it to WARN unless named explicitly.
      docker run -d --name "$name" ${LIMITS[@]+"${LIMITS[@]}"} -v "$volume:/data" \
        -e RUSTFS_ADDRESS=":$PORT" -e RUSTFS_REGION="$REGION" \
        -e RUSTFS_ACCESS_KEY="$ADMIN_KEY" -e RUSTFS_SECRET_KEY="$ADMIN_SECRET" \
        -e RUSTFS_OBS_LOG_STDOUT_ENABLED=true -e RUSTFS_OBS_LOG_DIRECTORY= \
        -e RUST_LOG='warn,rustfs::server::http=info' \
        ${RUSTFS_EXTRA[@]+$(printf -- '-e %s ' "${RUSTFS_EXTRA[@]}")} \
        "$RUSTFS_IMAGE" >/dev/null
      ;;
    versitygw)
      # The Kind chart's flags, plus the access log for the freeze probe.
      docker run -d --name "$name" ${LIMITS[@]+"${LIMITS[@]}"} -v "$volume:/data" \
        -e ROOT_ACCESS_KEY="$ADMIN_KEY" -e ROOT_SECRET_KEY="$ADMIN_SECRET" \
        "$VERSITYGW_IMAGE" --port ":$PORT" --health /healthz --region "$REGION" \
        --access-log - posix /data >/dev/null
      ;;
  esac
}

# Seconds from `docker run`/`start` returning control to the first 200 from
# the health endpoint, then to the first authenticated ListBuckets.
wait_ready() {
  local t0=$1 ep path t_health t_list
  ep="$(endpoint)"
  path="$(health_path)"
  until curl -sf -o /dev/null -m 1 "$ep$path"; do sleep 0.02; done
  t_health=$(date +%s.%N)
  until curl -sf -o /dev/null -m 1 --aws-sigv4 "aws:amz:$REGION:s3" --user "$ADMIN_KEY:$ADMIN_SECRET" "$ep/"; do sleep 0.02; done
  t_list=$(date +%s.%N)
  awk -v a="$t0" -v b="$t_health" -v c="$t_list" -v s="$store" -v e="$ep" \
    'BEGIN { printf "READY store=%s endpoint=%s health_s=%.3f s3_s=%.3f\n", s, e, b - a, c - a }'
}

case "${1:-}" in
  up)
    docker rm -f "$name" >/dev/null 2>&1 || true
    docker volume rm "$volume" >/dev/null 2>&1 || true
    docker volume create "$volume" >/dev/null
    t0=$(date +%s.%N)
    run_container
    wait_ready "$t0"
    ;;
  restart)
    docker stop -t 10 "$name" >/dev/null
    t0=$(date +%s.%N)
    docker start "$name" >/dev/null
    wait_ready "$t0"
    ;;
  down)
    docker rm -f "$name" >/dev/null 2>&1 || true
    docker volume rm "$volume" >/dev/null 2>&1 || true
    ;;
  env)
    printf 'export BAKEOFF_ENDPOINT=%s BAKEOFF_ACCESS_KEY=%s BAKEOFF_SECRET_KEY=%s BAKEOFF_VEND_ACCESS_KEY=%s BAKEOFF_VEND_SECRET_KEY=%s\n' \
      "$(endpoint)" "$ADMIN_KEY" "$ADMIN_SECRET" "$VEND_KEY" "$VEND_SECRET"
    ;;
  vend)
    [[ "$store" == rustfs ]] || die "only rustfs has users and policies to create"
    ep="$(endpoint)"
    sig=(--aws-sigv4 "aws:amz:$REGION:s3" --user "$ADMIN_KEY:$ADMIN_SECRET" -sf -o /dev/null)
    for b in snapshots templates; do
      curl "${sig[@]}" -X PUT "$ep/$b" || curl "${sig[@]}" -I "$ep/$b"
    done
    curl "${sig[@]}" -X PUT "$ep/rustfs/admin/v3/add-user?accessKey=$VEND_KEY" \
      -H 'content-type: application/json' -d "{\"secretKey\":\"$VEND_SECRET\",\"status\":\"enabled\"}"
    curl "${sig[@]}" -X PUT "$ep/rustfs/admin/v3/add-canned-policy?name=kind-storage-vend" \
      -H 'content-type: application/json' \
      -d '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject","s3:AbortMultipartUpload"],"Resource":["arn:aws:s3:::snapshots/*","arn:aws:s3:::templates/*"]},{"Effect":"Allow","Action":["s3:ListBucket"],"Resource":["arn:aws:s3:::snapshots","arn:aws:s3:::templates"]}]}'
    curl "${sig[@]}" -X POST "$ep/rustfs/admin/v3/idp/builtin/policy/attach" \
      -H 'content-type: application/json' -d "{\"policies\":[\"kind-storage-vend\"],\"user\":\"$VEND_KEY\"}" \
      || true # already attached
    echo "vend identity $VEND_KEY with policy kind-storage-vend on snapshots + templates"
    ;;
  veth)
    # The container's eth0 names its host-side peer by interface index; read
    # it from a helper sharing the container's network namespace, since the
    # store images need not ship cat.
    idx="$(docker run --rm --net "container:$name" busybox cat /sys/class/net/eth0/iflink)"
    ip -o link | awk -F': ' -v i="$idx" '$1 == i { sub("@.*", "", $2); print $2 }'
    ;;
  throttle)
    rate="${3:?rate, e.g. 240mbit}"
    veth="$("$0" veth "$store")"
    [[ -n "$veth" ]] || die "no veth found for $name"
    sudo tc qdisc replace dev "$veth" root tbf rate "$rate" burst 256kb latency 400ms
    echo "throttled $veth ($name ingress) to $rate"
    ;;
  unthrottle)
    veth="$("$0" veth "$store")"
    sudo tc qdisc del dev "$veth" root 2>/dev/null || true
    ;;
  *)
    sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
