#!/usr/bin/env bash
# REPRO ONLY: one end-to-end latent placeholder-grant stall in a fresh
# federated org, unattended.
#
#   run_latent_stall.sh <out-dir> <upstream-org> <subject> <placeholder-lifetime-s>
#
# 1. materialize the federated org (plaintext create) and enroll it in BYOK;
# 2. real-grant control create (also lazily provisions the org KEK);
# 3. warm both workers' intermediate-key caches, then create with `dummy`
#    (succeeds from the warm cache, so the placeholder is stored but latent);
# 4. native :stop, wait until the org's cache entries have expired;
# 5. native :start, then sample until the sandbox runs.
# Needs CALLER_KEY_FILE, ADMIN_TOKEN_FILE, NATIVE_KEY_FILE; broker double running.
set -euo pipefail
OUT="$1" ORG="$2" SUB="$3" LIFETIME="$4"
export OUT SANDBOX_TIMEOUT="${SANDBOX_TIMEOUT:-10800}"
mkdir -p "$OUT"
# shellcheck source=lib.sh
source "$(dirname "$0")/lib.sh"
BROKER_LOG=/tmp/byok-repro/broker.log

last_org_ok() {
  jq -r --arg org "$ORG" 'select(.decision == "OK" and (.tenant | endswith(":" + $org))) | .ts' "$BROKER_LOG" \
    | tail -1 | cut -d. -f1
}

note "[$ORG] materialize: $(fed_create "$ORG" "$SUB" real $(( $(date +%s) + SANDBOX_TIMEOUT )) | tee "$OUT/p0.txt" | tail -1)"
p0="$(head -1 "$OUT/p0.txt" | jq -r .sandboxID)"
space_org="$(space_org_for_upstream "$ORG")"
echo "$space_org" > "$OUT/space-org"
admin PATCH "orgs/$space_org" '{"byok_enabled":true}' | jq -c '{id, byok_enabled}' | tee -a "$OUT/timeline.log"
fed_request DELETE "/sandboxes/$p0" "$ORG" "$SUB" none 0 >/dev/null
note "[$ORG] control real-grant encrypted create: $(fed_create "$ORG" "$SUB" real $(( $(date +%s) + SANDBOX_TIMEOUT )) | tail -1)"

exp=$(( $(date +%s) + LIFETIME ))
echo "$exp" > "$OUT/placeholder-exp"
warm_then_create "$ORG" "$SUB" dummy "$exp" > "$OUT/d-create.txt"
id="$(grep -m1 sandboxID "$OUT/d-create.txt" | jq -r .sandboxID)"
echo "$id" > "$OUT/d-id"
note "[$ORG] placeholder create id=$id expires=$(date -u -d "@$exp" +%T)Z -> $(tail -1 "$OUT/d-create.txt")"
stored_grants "$space_org" | tee -a "$OUT/timeline.log"

note "[$ORG] :stop -> $(native POST "/v1/sandboxes/sbx_$id:stop" "$ORG" "$SUB" '{}' | head -1)"
until [[ "$(native GET "/v1/sandboxes/sbx_$id" "$ORG" "$SUB" | head -1 | jq -r .state)" == stopped ]]; do sleep 3; done
target=$(( $(last_org_ok) + 125 ))
while (( $(date +%s) < target )); do sleep 2; done
date +%s > "$OUT/start-t0"
note "[$ORG] :start (org cache cold) -> $(native POST "/v1/sandboxes/sbx_$id:start" "$ORG" "$SUB" '{}' | head -1)"
exec "$(dirname "$0")/record_stall.sh" "$OUT" "$ORG" "$SUB" "$id"
