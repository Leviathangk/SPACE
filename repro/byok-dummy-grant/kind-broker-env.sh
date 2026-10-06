#!/usr/bin/env bash
# REPRO ONLY: point the Kind credential authority at the grant-validating
# broker double (broker_double.py) and restore the standing Kind values.
#
#   kind-broker-env.sh apply   [broker-url]   # default http://172.18.0.1:50051
#   kind-broker-env.sh restore
#
# Kind's credmgr normally runs the in-memory KEK backend and never presents a
# federated grant. Selecting the intermediate-key broker as the platform default
# makes a keyless BYOK org lazily provision its KEK through the broker, so every
# DEK wrap/unwrap on a cold intermediate-key cache presents the sandbox's stored
# grant as AuthorizationEvidence. The API must agree on the platform default for
# its BYOK-enable gate. Only task-owned Kind workloads are touched; restore
# puts back the literals the charts render.
set -euo pipefail

CTX=kind-space-local
NS=space
k() { kubectl --context "$CTX" -n "$NS" "$@"; }

case "${1:-}" in
  apply)
    url="${2:-http://172.18.0.1:50051}"
    k set env ds/space-node-stack -c node-stack \
      SPACE_PLATFORM_KEK_PROVIDER=intermediate-key-broker \
      SPACE_CREDMGR_INTERMEDIATE_KEY_BROKER_URL="$url" \
      SPACE_CREDMGR_INTERMEDIATE_KEY_BROKER_TOKEN_FILE=/var/run/space-publication/cluster-api.token \
      SPACE_CREDMGR_INTERMEDIATE_KEY_BROKER_TENANT_NAMESPACE=urn:example:space-tenant \
      SPACE_CREDMGR_INTERMEDIATE_KEY_BROKER_GRANT_SCHEME=urn:example:space-grant:v1
    k set env deploy/space-api -c api SPACE_PLATFORM_KEK_PROVIDER=intermediate-key-broker
    ;;
  restore)
    k set env ds/space-node-stack -c node-stack \
      SPACE_PLATFORM_KEK_PROVIDER=in-memory \
      SPACE_CREDMGR_INTERMEDIATE_KEY_BROKER_URL- \
      SPACE_CREDMGR_INTERMEDIATE_KEY_BROKER_TOKEN_FILE- \
      SPACE_CREDMGR_INTERMEDIATE_KEY_BROKER_TENANT_NAMESPACE- \
      SPACE_CREDMGR_INTERMEDIATE_KEY_BROKER_GRANT_SCHEME-
    k set env deploy/space-api -c api SPACE_PLATFORM_KEK_PROVIDER=in-memory
    ;;
  *)
    echo "usage: $0 apply [broker-url] | restore" >&2
    exit 2
    ;;
esac
k rollout status ds/space-node-stack --timeout=600s
k rollout status deploy/space-api --timeout=600s
