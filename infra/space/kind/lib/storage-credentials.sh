#!/usr/bin/env bash
# storage-credentials.sh — how the Kind harness's node services get
# object-store credentials, selected by SPACE_KIND_STORAGE_CREDENTIALS for
# every bring-up path (the Tiltfile, bootstrap.sh and the object-store
# transition check):
#
#   static    (default) the versitygw object store; volume-manager,
#             build-manager and cluster-controller read its static keys.
#   exchange  the RustFS object store with STS. The credential-manager
#             authority vends each transfer a session scoped to its object
#             or artifact prefix through AssumeRole, as deployments do, and
#             volume-manager on the sandbox workers holds no object-store
#             keys. See "Object-store
#             credentials" in infra/space/kind/README.md.
#
# The two stores share no data, so the mode is fixed when a cluster is
# created (scripts/check-object-store-transition.sh).
#
# Source it for the function, or run it to print the mode.

# Prints the validated mode; fails on any other value.
space_kind_storage_credentials() {
  local mode="${SPACE_KIND_STORAGE_CREDENTIALS:-static}"
  case "$mode" in
    static|exchange) printf '%s\n' "$mode" ;;
    *)
      printf 'SPACE_KIND_STORAGE_CREDENTIALS must be static or exchange, not %q\n' "$mode" >&2
      return 1
      ;;
  esac
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  set -euo pipefail
  space_kind_storage_credentials
fi
