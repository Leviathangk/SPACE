#!/usr/bin/env bash
# negative-proof.sh <admin-token-file> [out-dir]
#
# Proves, through a node's real credential path, that a credential exchanged
# for one sandbox's snapshot does not reach another sandbox's. Needs a Kind
# cluster in exchange mode (SPACE_KIND_STORAGE_CREDENTIALS=exchange):
#
#   1. creates sandboxes A and B from seed:latest and suspends both, so each
#      has a snapshot in the snapshots bucket;
#   2. finds their snapshot keys (listing the bucket with the store's admin
#      keys through a port-forward: discovery only, nothing is checked with
#      them);
#   3. inside a sandbox node's volume-manager container, as volume-manager's
#      own uid and groups, calls the node credential-manager agent's volume
#      socket (ExchangeToken, exactly the request
#      crates/libs/vol/mgr/server/src/credentials.rs sends) with a static
#      grpcurl, so the request goes agent -> authority -> STS AssumeRole;
#   4. uses each issued credential with the aws CLI in that container against
#      the store, for A's objects (positive controls) and B's (must be
#      refused);
#   5. deletes both sandboxes.
#
# Prints a Markdown table; exits nonzero when any row does not hold.
set -uo pipefail
token_file="${1:?admin token file}"
out="${2:-/tmp/rustfs-bakeoff-negative-proof}"
mkdir -p "$out"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
ctx="kind-${SPACE_KIND_CLUSTER:-space-local}"
api_url="${SPACE_API_URL:-http://127.0.0.1:8080}"
token="$(<"$token_file")"
k() { kubectl --context "$ctx" -n space "$@"; }
log() { printf '[negative-proof] %s %s\n' "$(date -u +%T)" "$*" >&2; }

api() { # api METHOD PATH [BODY]
  local args=(-sS -X "$1" "$api_url$2" -H "Authorization: Bearer $token" -H 'Accept: application/json')
  local body="${3:-}"
  if [[ "$1" != GET ]]; then
    [[ -n "$body" ]] || body='{}'
    args+=(-H "Idempotency-Key: $(cat /proc/sys/kernel/random/uuid)" -H 'Content-Type: application/json' -d "$body")
  fi
  curl "${args[@]}"
}

wait_state() { # wait_state PATH STATE TIMEOUT
  local deadline=$((SECONDS + $3)) state
  while (( SECONDS < deadline )); do
    state="$(api GET "$1" | jq -r '.state // empty')"
    [[ "$state" == "$2" ]] && return 0
    [[ "$state" == failed || "$state" == canceled ]] && { log "$1 is $state"; return 1; }
    sleep 3
  done
  log "$1 did not reach $2 (last: ${state:-none})"
  return 1
}

# ── 1. Two suspended sandboxes ───────────────────────────────────────────────
declare -A sbx
for name in A B; do
  resp="$(api POST /v1/sandboxes '{"template":"seed:latest","runtime":"fc","name":"negative-proof-'"$name"'"}')"
  sbx[$name]="$(jq -r '.sandbox_id' <<<"$resp")"
  op="$(jq -r '.operation_id' <<<"$resp")"
  [[ "${sbx[$name]}" == sbx_* ]] || { log "create $name failed: $resp"; exit 1; }
  wait_state "/v1/operations/$op" succeeded 300 && wait_state "/v1/sandboxes/${sbx[$name]}" running 300 || exit 1
  log "sandbox $name ${sbx[$name]} running"
done
cleanup() {
  for name in A B; do
    [[ -n "${sbx[$name]:-}" ]] && api DELETE "/v1/sandboxes/${sbx[$name]}" >/dev/null 2>&1
  done
  [[ -n "${pf_pid:-}" ]] && kill "$pf_pid" 2>/dev/null
}
trap cleanup EXIT
for name in A B; do
  op="$(api POST "/v1/sandboxes/${sbx[$name]}:suspend" | jq -r '.operation_id // empty')"
  [[ -n "$op" ]] || { log "suspend $name refused"; exit 1; }
  wait_state "/v1/operations/$op" succeeded 600 && wait_state "/v1/sandboxes/${sbx[$name]}" suspended 120 || exit 1
  log "sandbox $name suspended"
done

# ── 2. Snapshot keys (discovery with the admin keys) ─────────────────────────
port=18333
k port-forward svc/object-store "$port:8333" >"$out/port-forward.log" 2>&1 &
pf_pid=$!
sleep 3
secret="$(k get secret space-object-store -o json)"
admin_env=(AWS_ACCESS_KEY_ID="$(jq -r '.data.SPACE_OBJECT_STORE_STATIC_ACCESS_KEY_ID | @base64d' <<<"$secret")"
  AWS_SECRET_ACCESS_KEY="$(jq -r '.data.SPACE_OBJECT_STORE_STATIC_SECRET_ACCESS_KEY | @base64d' <<<"$secret")"
  AWS_DEFAULT_REGION=us-east-1 AWS_EC2_METADATA_DISABLED=true)
env "${admin_env[@]}" aws --endpoint-url "http://127.0.0.1:$port" s3api list-objects-v2 --bucket snapshots \
  --query 'Contents[].Key' --output json >"$out/snapshot-keys.json" || { log "listing failed"; exit 1; }
declare -A memfile prefix
for name in A B; do
  uuid="${sbx[$name]#sbx_}"
  memfile[$name]="$(jq -r --arg u "$uuid" '[.[] | select(contains($u) and endswith("/memfile"))] | first // empty' "$out/snapshot-keys.json")"
  [[ -n "${memfile[$name]}" ]] || { log "no memfile for $name ($uuid)"; jq -r --arg u "$uuid" '.[] | select(contains($u))' "$out/snapshot-keys.json" >&2; exit 1; }
  prefix[$name]="${memfile[$name]%/memfile}"
  log "$name snapshot prefix ${prefix[$name]}"
done
other_a="$(jq -r --arg p "${prefix[A]}/" '[.[] | select(startswith($p) and (endswith("/memfile") | not))] | first // empty' "$out/snapshot-keys.json")"
subject_a="sandbox:${sbx[A]#sbx_}"

# ── 3/4. Exchange and use, inside each sandbox node's volume-manager ─────────
protos=("$repo/proto/credential/manager/v1/credential_manager.proto" "$repo/proto/credential/manager/v1/network_credential.proto")
rows=()
fail=0
row() { # row CASE EXPECTED OBSERVED
  local ok=no
  [[ "$3" == "$2"* ]] && ok=yes
  [[ "$ok" == no ]] && fail=1
  rows+=("| $1 | $2 | $3 | $ok |")
}

for pod in $(k get pods -l app.kubernetes.io/instance=space,space.pplx.net/node-service=volume-manager -o name); do
  pod="${pod#pod/}"
  node="$(k get pod "$pod" -o jsonpath='{.spec.nodeName}')"
  k exec "$pod" -c volume-manager -- mkdir -p /tmp/np/proto/credential/manager/v1
  k cp "$(command -v grpcurl)" "space/$pod:/tmp/np/grpcurl" -c volume-manager >/dev/null
  for p in "${protos[@]}"; do k cp "$p" "space/$pod:/tmp/np/proto/credential/manager/v1/$(basename "$p")" -c volume-manager >/dev/null; done
  k exec "$pod" -c volume-manager -- chmod 0755 /tmp/np/grpcurl

  # exchange SUBJECT RESOURCE SCOPE -> prints the response JSON or the error
  exchange() {
    jq -nc --arg r "$2" --arg s "$3" --arg sub "$1" '{grantType: "urn:ietf:params:oauth:grant-type:token-exchange",
      resource: $r, audience: "s3.amazonaws.com", scope: $s,
      requestedTokenType: "urn:ietf:params:oauth:token-type:aws-credentials",
      subjectToken: $sub, subjectTokenType: "urn:space:token-type:subject-id", requestedExpiresIn: 3600}' |
      k exec -i "$pod" -c volume-manager -- /tmp/np/grpcurl -plaintext -import-path /tmp/np/proto \
        -proto credential/manager/v1/credential_manager.proto -d @ \
        unix:///space/run/volume-credentials/credmgr.sock space.credential.manager.v1.CredentialManager/ExchangeToken 2>&1
  }
  issued() { jq -e '.awsCredentials.accessKeyId' >/dev/null 2>&1 <<<"$1" && echo issued || echo "refused: $(tr '\n' ' ' <<<"$1" | grep -oE 'Code: [A-Za-z]+' | head -1)"; }
  # s3 CREDS-JSON ARGS... -> "200" or the S3 error code, from inside the pod
  s3() {
    local c="$1"; shift
    k exec "$pod" -c volume-manager -- env HOME=/tmp/np AWS_EC2_METADATA_DISABLED=true AWS_DEFAULT_REGION=us-east-1 \
      AWS_ACCESS_KEY_ID="$(jq -r .awsCredentials.accessKeyId <<<"$c")" \
      AWS_SECRET_ACCESS_KEY="$(jq -r .awsCredentials.secretAccessKey <<<"$c")" \
      AWS_SESSION_TOKEN="$(jq -r .awsCredentials.sessionToken <<<"$c")" \
      aws --endpoint-url http://object-store:8333 s3api "$@" >/tmp/np-s3.out 2>&1
    local rc=$?
    # HeadObject errors carry no body, so the CLI reports the bare status.
    if (( rc == 0 )); then echo 200; else grep -oE '\(([A-Za-z]+|[0-9]{3})\)' /tmp/np-s3.out | head -1 | tr -d '()' | sed 's/^403$/AccessDenied (403)/'; fi
  }

  read_a="$(exchange "$subject_a" "arn:aws:s3:::snapshots/${memfile[A]}" space.storage.read)"
  row "[$node] read exchange: subject A, A's memfile (exact object)" issued "$(issued "$read_a")"
  if [[ "$(issued "$read_a")" == issued ]]; then
    row "[$node] read session: HeadObject A's memfile" 200 "$(s3 "$read_a" head-object --bucket snapshots --key "${memfile[A]}")"
    row "[$node] read session: GetObject A's memfile, Range 0-0" 200 "$(s3 "$read_a" get-object --bucket snapshots --key "${memfile[A]}" --range bytes=0-0 /tmp/np/body)"
    row "[$node] read session: ListBucket prefix = A's memfile" 200 "$(s3 "$read_a" list-objects-v2 --bucket snapshots --prefix "${memfile[A]}")"
    row "[$node] read session: HeadObject B's memfile" AccessDenied "$(s3 "$read_a" head-object --bucket snapshots --key "${memfile[B]}")"
    row "[$node] read session: GetObject B's memfile" AccessDenied "$(s3 "$read_a" get-object --bucket snapshots --key "${memfile[B]}" /tmp/np/body)"
    row "[$node] read session: ListBucket prefix = B's snapshot" AccessDenied "$(s3 "$read_a" list-objects-v2 --bucket snapshots --prefix "${prefix[B]}/")"
    [[ -n "$other_a" ]] && row "[$node] read session: GetObject another of A's files ($(basename "$other_a"))" AccessDenied "$(s3 "$read_a" get-object --bucket snapshots --key "$other_a" /tmp/np/body)"
    row "[$node] read session: PutObject A's memfile (write under read-only)" AccessDenied "$(s3 "$read_a" put-object --bucket snapshots --key "${memfile[A]}" --body /tmp/np/grpcurl)"
  fi
  # A prefix write session, as volume-manager's artifact pushes request one.
  write_a="$(exchange "$subject_a" "arn:aws:s3:::snapshots/${prefix[A]}/*" "space.storage.read space.storage.write")"
  w="$(issued "$write_a")"
  rows+=("| [$node] write exchange: subject A, A's snapshot prefix | issued on A's node; refused elsewhere | $w | (recorded) |")
  if [[ "$w" == issued ]]; then
    row "[$node] write session: PutObject under A's prefix" 200 "$(s3 "$write_a" put-object --bucket snapshots --key "${prefix[A]}/negative-proof" --body /tmp/np/proto/credential/manager/v1/network_credential.proto)"
    row "[$node] write session: PutObject under B's prefix" AccessDenied "$(s3 "$write_a" put-object --bucket snapshots --key "${prefix[B]}/negative-proof" --body /tmp/np/proto/credential/manager/v1/network_credential.proto)"
    row "[$node] write session: PutObject over B's memfile" AccessDenied "$(s3 "$write_a" put-object --bucket snapshots --key "${memfile[B]}" --body /tmp/np/proto/credential/manager/v1/network_credential.proto)"
    row "[$node] write session: CreateMultipartUpload under B's prefix" AccessDenied "$(s3 "$write_a" create-multipart-upload --bucket snapshots --key "${prefix[B]}/negative-proof-mpu")"
    row "[$node] write session: PutObject in the templates bucket" AccessDenied "$(s3 "$write_a" put-object --bucket templates --key "${prefix[A]}/negative-proof" --body /tmp/np/proto/credential/manager/v1/network_credential.proto)"
  fi
  # The authority itself must refuse naming B's object for subject A.
  row "[$node] read exchange: subject A, B's memfile" refused "$(issued "$(exchange "$subject_a" "arn:aws:s3:::snapshots/${memfile[B]}" space.storage.read)")"
  k exec "$pod" -c volume-manager -- rm -rf /tmp/np /tmp/np-s3.out
done

{
  echo "| Case | Expected | Observed | Holds |"
  echo "|---|---|---|---|"
  printf '%s\n' "${rows[@]}"
} | tee "$out/negative-proof.md"
exit "$fail"
