# shellcheck shell=bash
# REPRO ONLY helpers for the placeholder-grant repro on the Kind harness.
# Source from bash. Needs: curl, jq, kubectl (context kind-space-local).
#
# Inputs:
#   CALLER_KEY_FILE  raw ECW key of a trusted federated caller
#                    (api_keys.federated_owner_assertions_enabled = true)
#   ADMIN_TOKEN_FILE admin session bearer (POST /auth/v1/sessions as root@local)
#   OUT              evidence directory

REPRO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
API_URL="${API_URL:-http://localhost:8080}"
ECW_API_HOST="${ECW_API_HOST:-api.local.e2b.test}"
OUT="${OUT:-/tmp/byok-repro/run}"
mkdir -p "$OUT"

k() { kubectl --context kind-space-local -n space "$@"; }
psql_q() { "$REPRO_DIR/psql.sh" -tA -F '|' -c "$1"; }
now() { date -u +%Y-%m-%dT%H:%M:%SZ; }
note() { printf '%s %s\n' "$(now)" "$*" | tee -a "$OUT/timeline.log"; }

# Synthetic "real" grant: base64url(JSON{sub,org,exp}). The broker double
# accepts it while exp is in the future and sub/org match the request.
real_grant() { # upstream-org subject expires-at-unix
  printf '{"sub":"%s","org":"%s","exp":%d}' "$2" "$1" "$3" \
    | base64 -w0 | tr '+/' '-_' | tr -d '='
}

# ECW request as the trusted federated caller, asserting an upstream owner.
#   GRANT_MODE: real | dummy | none
fed_request() { # method path upstream-org subject grant-mode expires-at-unix [body]
  local method="$1" path="$2" org="$3" sub="$4" mode="$5" exp="$6" body="${7:-}"
  local args=(-sS -m "${CURL_TIMEOUT:-300}" -X "$method" "$API_URL$path"
    -H "Host: $ECW_API_HOST" -H 'content-type: application/json'
    -H "x-space-federated-org-id: $org" -H "x-space-federated-user-id: $sub"
    -w '\n%{http_code} %{time_total}')
  case "$mode" in
    real)  args+=(-H "x-space-federated-service-grant: $(real_grant "$org" "$sub" "$exp")"
                  -H "x-space-federated-service-grant-expires-at: $exp") ;;
    dummy) args+=(-H "x-space-federated-service-grant: dummy"
                  -H "x-space-federated-service-grant-expires-at: $exp") ;;
    none)  ;;
    *) echo "bad grant mode $mode" >&2; return 2 ;;
  esac
  [[ -n "$body" ]] && args+=(-d "$body")
  printf 'header = "x-api-key: %s"\n' "$(<"$CALLER_KEY_FILE")" | curl --config - "${args[@]}"
}

# SANDBOX_TIMEOUT (default 3600) is the ECW sandbox lifetime; keep it longer
# than any stall being observed, or the timeout deletes the sandbox and its grant.
fed_create() { # upstream-org subject grant-mode expires-at-unix
  fed_request POST /sandboxes "$1" "$2" "$3" "$4" "{\"templateID\":\"seed\",\"timeout\":${SANDBOX_TIMEOUT:-3600}}"
}

admin() { # method resource [body]
  local args=(-fsS -X "$1" "$API_URL/admin/api/v1/$2"
    -H "Authorization: Bearer $(<"$ADMIN_TOKEN_FILE")" -H 'Content-Type: application/json'
    -H "Idempotency-Key: $(cat /proc/sys/kernel/random/uuid)")
  [[ $# -ge 3 ]] && args+=(-d "$3")
  curl "${args[@]}"
}

space_org_for_upstream() { # upstream-org
  psql_q "SELECT id FROM organizations WHERE owner_source='federated_identity' AND upstream_org_id='$1'"
}

# Stored grants for an org, with the blob shown only as length + whether it is
# the literal placeholder (the synthetic real grants are not secret either).
stored_grants() { # space-org-id
  psql_q "SELECT g.sandbox_id, octet_length(g.grant_blob) AS len,
                 (g.grant_blob = 'dummy'::bytea) AS is_placeholder,
                 g.grant_expires_at, g.grant_expires_at > now() AS live, s.lifecycle, s.active_node_id
          FROM sandbox_federated_service_grants g JOIN sandboxes s ON s.id = g.sandbox_id
          WHERE s.org_id = '$1' ORDER BY g.grant_expires_at"
}

broker_log_since() { # unix-ts
  jq -c --argjson t "$1" 'select(.ts >= $t)' /tmp/byok-repro/broker.log 2>/dev/null
}

node_logs_grep() { # pattern [service-log-name...]
  local pattern="$1"; shift
  local p
  for p in $(k get pods -l space.pplx.net/chart=space-node-stack -o name | grep -v builder); do
    k exec "$p" -c node-stack -- bash -c "cd /space/logs && grep -h -E '$pattern' ${*:-credmgr.log nodemgr.log sbxmgr.log} 2>/dev/null" \
      | sed "s|^|${p#pod/} |"
  done
}

org_sandboxes() { # space-org-id
  psql_q "SELECT s.id, s.lifecycle, s.placement, host(n.address) AS node_ip,
                 (sve.sandbox_id IS NOT NULL) AS encrypted
          FROM sandboxes s
          LEFT JOIN sandbox_volume_encryption sve ON sve.sandbox_id = s.id
          LEFT JOIN nodes n ON n.id = s.active_node_id
          WHERE s.org_id = '$1' ORDER BY s.created_at"
}

org_kek() { # space-org-id
  psql_q "SELECT provider, key_identifier, retired_at FROM org_master_keys WHERE org_id = '$1'"
}

# Warm every worker's intermediate-key cache for the org, then create with the
# given grant mode immediately. Each worker's credmgr authority keeps its own
# cache (min of 900 s local, broker cap, grant lifetime, minus jitter), so a
# placeholder-grant create only survives when it lands on a warm node.
# Prints the created sandbox's create response line. Warm-up sandboxes are
# left running so the org keeps live real grants; callers delete them.
warm_then_create() { # upstream-org subject grant-mode expires-at-unix
  local org="$1" sub="$2" mode="$3" exp="$4" space_org seen i id ip
  space_org="$(space_org_for_upstream "$org")"
  declare -A seen=()
  for i in 1 2 3 4 5 6 7 8; do
    id="$(fed_create "$org" "$sub" real $(( $(date +%s) + 3600 )) | head -1 | jq -r '.sandboxID // empty')"
    [[ -n "$id" ]] || continue
    ip="$(psql_q "SELECT host(n.address) FROM sandboxes s JOIN nodes n ON n.id = s.active_node_id WHERE s.id = '$id'")"
    note "warm-up $i: $id on $ip"
    echo "$id" >> "$OUT/warmups"
    seen[$ip]=1
    (( ${#seen[@]} >= 2 )) && break
  done
  note "warm nodes: ${!seen[*]}; creating with grant=$mode exp=$exp"
  fed_create "$org" "$sub" "$mode" "$exp"
}

# Native /v1 call as the trusted federated caller (no grant headers, so the
# sandbox's stored grant is what credmgr presents).
NATIVE_KEY_FILE="${NATIVE_KEY_FILE:-/tmp/byok-repro/caller-native-key}"
native() { # method path upstream-org subject [body]
  local args=(-sS -m 60 -X "$1" "$API_URL$2"
    -H "Authorization: Bearer $(<"$NATIVE_KEY_FILE")"
    -H "x-space-federated-org-id: $3" -H "x-space-federated-user-id: $4"
    -H 'content-type: application/json'
    -H "Idempotency-Key: $(cat /proc/sys/kernel/random/uuid)"
    -w '\n%{http_code} %{time_total}')
  [[ $# -ge 5 ]] && args+=(-d "$5")
  curl "${args[@]}"
}
