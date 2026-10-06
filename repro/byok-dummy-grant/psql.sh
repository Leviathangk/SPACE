#!/usr/bin/env bash
# Read-mostly psql into the task-owned Kind control-plane database (same path
# as scripts/tests/lib/bootstrap-kind-stack.sh kind_stack_psql).
set -euo pipefail
k() { kubectl --context kind-space-local -n space "$@"; }
creds="$(k get secret space-postgres -o go-template='{{index .data "POSTGRES_USER" | base64decode}} {{index .data "POSTGRES_DB" | base64decode}}')"
read -r user db <<<"$creds"
k exec -i sts/space-postgres -- psql -U "$user" -d "$db" -v ON_ERROR_STOP=1 "$@"
