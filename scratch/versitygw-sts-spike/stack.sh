#!/usr/bin/env bash
# stack.sh — versitygw v1.9.0 with its standalone IAM service, wired so the
# S3 gateway accepts the sessions the IAM service issues, plus the STS bridge
# prototype, which is also the test OIDC issuer. All on one Docker host, all
# credentials synthetic local-dev values.
#
#   stack.sh up      fresh keys, network, containers, buckets, OIDC provider and roles
#   stack.sh setup   (re)create buckets, the OIDC provider and the roles only
#   stack.sh down    remove containers, volumes and the network
#   stack.sh env     print the probe's environment (BAKEOFF_* and SPIKE_*)
#   stack.sh restart-gw   restart the S3 gateway on its data, print start → ready
#
# Containers (network vsts):
#   vsts-iam     versitygw iam: IAM/STS API on :7070, private API on a unix
#                socket shared with the gateway through the vsts-sock volume
#   vsts-gw      versitygw posix gateway on :8333, --iam-standalone-endpoint
#                naming that socket; the Kind chart's limits (1 CPU, 4 GiB)
#   vsts-bridge  bridge serve: STS AssumeRole on :8080, issuer on :8443 (TLS)
#
# Needs docker, go (builds the bridge), the aws CLI and curl.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSITYGW_IMAGE="${VERSITYGW_IMAGE:-ghcr.io/versity/versitygw:v1.9.0@sha256:eb7f786fcd91e72194e48fcf1ac16302d686bf8e3a71434bdbc067a6efd3fdc7}"
BRIDGE_BASE_IMAGE="${BRIDGE_BASE_IMAGE:-busybox:latest}"
STATE="${SPIKE_STATE:-/tmp/versitygw-sts-spike}"
ADMIN_KEY=localadmin
ADMIN_SECRET=localadmin-dev-secret
VEND_KEY=localvend
VEND_SECRET=localvend-dev-secret
REGION=us-east-1
NET=vsts
# The issuer every minted token names. It never resolves: the IAM service
# fetches its discovery document from the bridge container instead
# (--oidc-discovery-url), so no private-endpoint relaxation is needed.
ISSUER_HOST=sts-bridge.kind.invalid
ISSUER="https://$ISSUER_HOST"
DISCOVERY="https://vsts-bridge:8443/.well-known/openid-configuration"
AUDIENCE=kind-sts-bridge
SUBJECT=kind-storage-vend
ACCOUNT=000000000000
ROLE=kind-storage-vend
OTHER_ROLE=kind-storage-other
PROVIDER_ARN="arn:aws:iam::$ACCOUNT:oidc-provider/$ISSUER_HOST"
read -r -a GW_LIMITS <<<"${SPIKE_GW_LIMITS---cpus 1 --memory 4g}"
read -r -a IAM_LIMITS <<<"${SPIKE_IAM_LIMITS---cpus 1 --memory 1g}"

die() { printf 'stack.sh: %s\n' "$*" >&2; exit 1; }
ip_of() { docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$1"; }
iam() {
  AWS_ACCESS_KEY_ID=$ADMIN_KEY AWS_SECRET_ACCESS_KEY=$ADMIN_SECRET AWS_REGION=$REGION \
    aws --endpoint-url "http://$(ip_of vsts-iam):7070" --output json iam "$@"
}
s3() {
  AWS_ACCESS_KEY_ID=$ADMIN_KEY AWS_SECRET_ACCESS_KEY=$ADMIN_SECRET AWS_REGION=$REGION \
    aws --endpoint-url "http://$(ip_of vsts-gw):8333" s3api "$@"
}
wait_http() {
  local url=$1 n=0
  until curl -skf -o /dev/null -m 1 "$url"; do
    n=$((n + 1)); [[ $n -lt 250 ]] || die "timed out waiting for $url"; sleep 0.02
  done
}

run_iam() {
  docker run -d --name vsts-iam --network $NET ${IAM_LIMITS[@]+"${IAM_LIMITS[@]}"} \
    -v vsts-iam-data:/iam -v vsts-sock:/sock \
    -e ROOT_ACCESS_KEY=$ADMIN_KEY -e ROOT_SECRET_KEY=$ADMIN_SECRET \
    "$VERSITYGW_IMAGE" --port :7070 --health /healthz --region $REGION \
    iam --dir /iam --private-ports /sock/iam.sock \
    --oidc-discovery-url "$ISSUER=$DISCOVERY" >/dev/null
}

run_gw() {
  docker run -d --name vsts-gw --network $NET ${GW_LIMITS[@]+"${GW_LIMITS[@]}"} \
    -v vsts-gw-data:/data -v vsts-sock:/sock \
    -e ROOT_ACCESS_KEY=$ADMIN_KEY -e ROOT_SECRET_KEY=$ADMIN_SECRET \
    "$VERSITYGW_IMAGE" --port :8333 --health /healthz --region $REGION --access-log - \
    --iam-standalone-endpoint /sock/iam.sock posix /data >/dev/null
}

# The Docker daemon need not share this host's filesystem, so the binary and
# keys are copied in rather than bind-mounted.
run_bridge() {
  docker create --name vsts-bridge --network $NET \
    -e BRIDGE_IDENTITIES="$VEND_KEY:$VEND_SECRET:$SUBJECT" \
    "$BRIDGE_BASE_IMAGE" /bridge serve -listen :8080 -issuer-listen :8443 \
    -tls-cert /keys/tls.pem -tls-key /keys/tls.key -signing-key /keys/signing.key \
    -issuer "$ISSUER" -jwks-base https://vsts-bridge:8443 -audience $AUDIENCE \
    -upstream http://vsts-iam:7070/ >/dev/null
  docker cp -q "$STATE/bridge" vsts-bridge:/bridge
  docker cp -q "$STATE/keys" vsts-bridge:/keys
  docker start vsts-bridge >/dev/null
}

# The role's identity policy: what the RustFS bake-off gave the vend user,
# read, write and abort uploads in snapshots and templates, and list them.
VEND_POLICY='{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject","s3:AbortMultipartUpload"],"Resource":["arn:aws:s3:::snapshots/*","arn:aws:s3:::templates/*"]},{"Effect":"Allow","Action":["s3:ListBucket"],"Resource":["arn:aws:s3:::snapshots","arn:aws:s3:::templates"]}]}'

trust_for() {
  printf '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Federated":"%s"},"Action":"sts:AssumeRoleWithWebIdentity","Condition":{"StringEquals":{"%s:sub":"%s","%s:aud":"%s"}}}]}' \
    "$PROVIDER_ARN" "$ISSUER_HOST" "$1" "$ISSUER_HOST" "$AUDIENCE"
}

setup() {
  for b in snapshots templates bakeoff-unvended; do
    s3 head-bucket --bucket $b >/dev/null 2>&1 || s3 create-bucket --bucket $b >/dev/null
  done
  iam delete-open-id-connect-provider --open-id-connect-provider-arn "$PROVIDER_ARN" >/dev/null 2>&1 || true
  # A second client ID the trust policy does not name: the trust check, not
  # only the provider's list, must refuse it.
  iam create-open-id-connect-provider --url "$ISSUER" --client-id-list $AUDIENCE kind-other-client \
    --thumbprint-list "$(cat "$STATE/keys/ca.sha1")" >/dev/null
  for r in "$ROLE:$SUBJECT" "$OTHER_ROLE:someone-else"; do
    name=${r%%:*} sub=${r#*:}
    iam delete-role-policy --role-name "$name" --policy-name vend >/dev/null 2>&1 || true
    iam delete-role --role-name "$name" >/dev/null 2>&1 || true
    iam create-role --role-name "$name" --max-session-duration 3600 \
      --assume-role-policy-document "$(trust_for "$sub")" >/dev/null
    iam put-role-policy --role-name "$name" --policy-name vend --policy-document "$VEND_POLICY" >/dev/null
  done
  echo "provider $PROVIDER_ARN (aud $AUDIENCE, thumbprint $(cat "$STATE/keys/ca.sha1"))"
  echo "role $ROLE trusts sub $SUBJECT; role $OTHER_ROLE trusts sub someone-else; both max 3600 s"
}

case "${1:-}" in
  up)
    "$0" down
    mkdir -p "$STATE"
    (cd "$here/bridge" && CGO_ENABLED=0 go build -o "$STATE/bridge" .)
    rm -rf "$STATE/keys"
    "$STATE/bridge" keygen -dir "$STATE/keys" -hosts vsts-bridge,localhost
    docker network create $NET >/dev/null
    for v in vsts-iam-data vsts-gw-data vsts-sock; do docker volume create $v >/dev/null; done
    run_bridge
    run_iam
    wait_http "http://$(ip_of vsts-iam):7070/healthz"
    t0=$(date +%s.%N)
    run_gw
    wait_http "http://$(ip_of vsts-gw):8333/healthz"
    awk -v a="$t0" -v b="$(date +%s.%N)" 'BEGIN { printf "gateway ready in %.3f s\n", b - a }'
    wait_http "https://$(ip_of vsts-bridge):8443/jwks.json"
    setup
    ;;
  setup) setup ;;
  restart-gw)
    docker stop -t 10 vsts-gw >/dev/null
    t0=$(date +%s.%N)
    docker start vsts-gw >/dev/null
    wait_http "http://$(ip_of vsts-gw):8333/healthz"
    awk -v a="$t0" -v b="$(date +%s.%N)" 'BEGIN { printf "gateway ready in %.3f s\n", b - a }'
    ;;
  down)
    docker rm -f vsts-gw vsts-iam vsts-bridge >/dev/null 2>&1 || true
    for v in vsts-iam-data vsts-gw-data vsts-sock; do docker volume rm $v >/dev/null 2>&1 || true; done
    docker network rm $NET >/dev/null 2>&1 || true
    ;;
  env)
    printf 'export BAKEOFF_ENDPOINT=http://%s:8333 BAKEOFF_ACCESS_KEY=%s BAKEOFF_SECRET_KEY=%s BAKEOFF_VEND_ACCESS_KEY=%s BAKEOFF_VEND_SECRET_KEY=%s\n' \
      "$(ip_of vsts-gw)" $ADMIN_KEY $ADMIN_SECRET $VEND_KEY $VEND_SECRET
    printf 'export SPIKE_IAM_ENDPOINT=http://%s:7070 SPIKE_BRIDGE_ENDPOINT=http://%s:8080 SPIKE_MINT="%s mint -signing-key %s" SPIKE_OTHER_KEY=%s SPIKE_ISSUER=%s SPIKE_AUDIENCE=%s SPIKE_SUBJECT=%s\n' \
      "$(ip_of vsts-iam)" "$(ip_of vsts-bridge)" "$STATE/bridge" "$STATE/keys/signing.key" "$STATE/keys/other.key" "$ISSUER" $AUDIENCE $SUBJECT
    ;;
  *)
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
