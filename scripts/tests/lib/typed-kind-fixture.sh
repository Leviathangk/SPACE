# shellcheck shell=bash
# Shared fixture preparation for the typed Kind runners.

require_native_token() {
  if [[ -z "${SPACE_API_TOKEN:-}" && -n "${SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE:-}" ]]; then
    SPACE_API_TOKEN="$(<"$SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE")"
    export SPACE_API_TOKEN
  fi
  if [[ -z "${SPACE_API_TOKEN:-}" ]]; then
    echo "ERROR: the native API surface needs SPACE_API_TOKEN or SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE" >&2
    exit 2
  fi
  # The native client defaults its data plane to the API URL; on Kind the public data
  # plane is port-forwarded separately (same default the ECW surface uses).
  export SPACE_DATA_PLANE_URL="${SPACE_DATA_PLANE_URL:-http://127.0.0.1:8090}"
}

# Recovery participates in the normal native inventory on this dedicated Kind
# bench. Snapshot its identity here; every mutation revalidates that identity.
prepare_native_kind() {
  require_native_token
  local target="kind-${KIND_CLUSTER:-space-local}"
  if [[ -n "${SPACE_SMOKE_NATIVE_RECOVERY_CONTEXT:-}" && "$SPACE_SMOKE_NATIVE_RECOVERY_CONTEXT" != "$target" ]]; then
    echo "ERROR: native recovery context does not match KIND_CLUSTER" >&2
    exit 2
  fi
  local uid
  uid="$(kubectl --context "$target" get namespace kube-system -o jsonpath='{.metadata.uid}')"
  if [[ -z "$uid" || ( -n "${SPACE_SMOKE_NATIVE_RECOVERY_CLUSTER_UID:-}" && "$SPACE_SMOKE_NATIVE_RECOVERY_CLUSTER_UID" != "$uid" ) ]]; then
    echo "ERROR: native recovery cluster identity does not match the Kind bench" >&2
    exit 2
  fi
  export SPACE_SMOKE_NATIVE_RECOVERY=1
  export SPACE_SMOKE_NATIVE_TENANT_FIXTURES=1
  export SPACE_SMOKE_NATIVE_RECOVERY_CONTEXT="$target"
  export SPACE_SMOKE_NATIVE_RECOVERY_CLUSTER_UID="$uid"
}

# `local.private_template_build_isolation` needs a second organization with a
# service user. Organizations are not per-run fixtures (a deleted template keeps
# its ownership record), so one stable organization per bench, found by slug or
# created on first use, serves every run. Without an operator token this does
# nothing and the scenario reports its own missing input.
BUILD_FOREIGN_ORG_SLUG="space-smoke-build-foreign"

ensure_build_foreign_org() {
  [[ -n "${SPACE_SMOKE_BUILD_FOREIGN_ORG_ID:-}" ]] && return 0
  local token="${SPACE_API_TOKEN:-}"
  if [[ -z "$token" && -n "${SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE:-}" ]]; then
    token="$(<"$SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE")"
  fi
  [[ -n "$token" ]] || return 0
  for bin in curl jq; do
    if ! command -v "$bin" >/dev/null 2>&1; then
      echo "ERROR: $bin is required to provision the build test organization" >&2
      exit 2
    fi
  done
  local admin="${API_URL:-http://127.0.0.1:8080}/admin/api/v1"
  admin_call() {
    local args=(-fsS -X "$1" "$admin/$2"
      -H "Authorization: Bearer $token"
      -H 'Content-Type: application/json'
      -H "Idempotency-Key: $(cat /proc/sys/kernel/random/uuid)")
    if [[ $# -ge 3 ]]; then
      args+=(-d "$3")
    fi
    curl "${args[@]}"
  }
  local org_id
  org_id="$(admin_call GET orgs | jq -r --arg slug "$BUILD_FOREIGN_ORG_SLUG" \
    'first(.[] | select(.slug == $slug) | .id) // empty')"
  if [[ -z "$org_id" ]]; then
    org_id="$(admin_call POST orgs "$(jq -nc --arg slug "$BUILD_FOREIGN_ORG_SLUG" \
      '{name: "space-smoke build test organization", slug: $slug}')" | jq -r '.id')"
  fi
  if [[ "$(admin_call GET "orgs/$org_id/users" | jq 'length')" == 0 ]]; then
    # The scenario seeds its own API keys for this user; the password is never used.
    admin_call POST "orgs/$org_id/users" "$(jq -nc \
      --arg email "service@${BUILD_FOREIGN_ORG_SLUG}.local" \
      --arg password "$(head -c 32 /dev/urandom | base64 | tr -dc 'A-Za-z0-9')" \
      '{email: $email, password: $password, display_name: "space-smoke build service user", role: "member"}')" \
      >/dev/null
  fi
  export SPACE_SMOKE_BUILD_FOREIGN_ORG_ID="$org_id"
  echo "--- build test organization ${BUILD_FOREIGN_ORG_SLUG}: ${org_id}"
}
