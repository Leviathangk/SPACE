#!/usr/bin/env bash
# REPRO ONLY: sample a stalled encrypted sandbox every 30 s until it starts.
#   record_stall.sh <out-dir> <upstream-org> <subject> <sandbox-uuid>
# Appends to <out-dir>/samples.log: public state, control-plane lifecycle,
# sandbox pod(s), and whether the sandbox's own stored grant is still the live
# placeholder. Broker decisions are in the broker double's own log.
set -uo pipefail
OUT="$1" ORG="$2" SUB="$3" ID="$4"
export OUT
# shellcheck source=lib.sh
source "$(dirname "$0")/lib.sh"
pod_key="${ID//-/}"
space_org="$(space_org_for_upstream "$ORG")"
while true; do
  state="$(native GET "/v1/sandboxes/sbx_$ID" "$ORG" "$SUB" | head -1 | jq -r .state)"
  life="$(psql_q "SELECT lifecycle FROM sandboxes WHERE id = '$ID'")"
  pods="$(k get pods --no-headers 2>/dev/null | grep "$pod_key" | awk '{print $1, $3}' | tr '\n' ' ')"
  grant="$(stored_grants "$space_org" | grep "^$ID" | cut -d'|' -f3,5)"
  echo "$(now) public_state=$state lifecycle=$life pods=[$pods] own_grant(placeholder|live)=${grant:-none}" >> "$OUT/samples.log"
  if [[ "$state" == started ]]; then
    echo "$(now) RECOVERED public_state=started" >> "$OUT/samples.log"
    break
  fi
  sleep 30
done
