#!/usr/bin/env bash
# check-object-store-transition.sh — refuse a kind cluster whose object store
# this bring-up would replace: one that still runs the SeaweedFS chart the
# stack replaced with versitygw, or one created with the other storage
# credentials mode (SPACE_KIND_STORAGE_CREDENTIALS,
# infra/space/kind/lib/storage-credentials.sh), whose store is a different
# server over a different data directory.
#
# The two stores share nothing: versitygw serves a new data directory behind
# a new Service (`object-store:8333`), while a SeaweedFS-era cluster keeps its
# suspend snapshots and published templates in SeaweedFS, and its database
# records their storage locations as `http://seaweedfs:8333`. Applying the
# new charts to such a cluster would leave every existing restore and template
# pull depending on the old deployment, so every bring-up path stops here
# instead, and the operator recreates the cluster. See "Object store
# transition" in infra/space/kind/README.md.
#
# The same holds between the two storage credentials modes: `static` runs
# versitygw and `exchange` runs SeaweedFS, both behind `object-store:8333`,
# so recorded locations would resolve to a store that never held the objects.
# The object store's Deployment carries the mode it was created with; one
# without the label predates the modes and ran versitygw, which is `static`.
#
# Read-only. Exits 0 when the cluster does not exist, or carries no resource of
# the old `space-seaweedfs` chart and no object store from the other mode; 1
# (with the remedy on stderr) when it does, when the mode is invalid, or when
# the cluster cannot be read.
#
# Usage:
#   bash infra/space/kind/scripts/check-object-store-transition.sh [cluster]
#
# The cluster defaults to $SPACE_KIND_CLUSTER, then space-local.
set -euo pipefail

KIND_CLUSTER="${1:-${SPACE_KIND_CLUSTER:-space-local}}"
NAMESPACE=space
# shellcheck source=../lib/storage-credentials.sh
. "$(dirname "${BASH_SOURCE[0]}")/../lib/storage-credentials.sh"
requested_mode="$(space_kind_storage_credentials)" || exit 1

kind get clusters 2>/dev/null | grep -qx "$KIND_CLUSTER" || exit 0

env_prefix=""
[[ "$KIND_CLUSTER" == space-local ]] || env_prefix="SPACE_KIND_CLUSTER=$KIND_CLUSTER "

# Every resource the old chart rendered carries this label, its Service
# (`seaweedfs`) included. A namespace that does not exist yet lists nothing.
if ! legacy="$(kubectl --context "kind-$KIND_CLUSTER" -n "$NAMESPACE" \
    get deployments,services,secrets,jobs,pods \
    -l app.kubernetes.io/name=space-seaweedfs -o name 2>&1)"; then
  printf 'ERROR: cannot check kind cluster %s for the old SeaweedFS object store:\n%s\n' \
    "$KIND_CLUSTER" "$legacy" >&2
  exit 1
fi
if [[ -z "$legacy" ]]; then
  if ! deployed_mode="$(kubectl --context "kind-$KIND_CLUSTER" -n "$NAMESPACE" \
      get deployments -l app.kubernetes.io/name=space-object-store \
      -o jsonpath='{range .items[*]}{.metadata.labels.space\.pplx\.net/kind-storage-credentials}{"|"}{end}' 2>&1)"; then
    printf 'ERROR: cannot read the object store of kind cluster %s:\n%s\n' \
      "$KIND_CLUSTER" "$deployed_mode" >&2
    exit 1
  fi
  # No Deployment yet: nothing to switch.
  [[ -z "$deployed_mode" ]] && exit 0
  deployed_mode="${deployed_mode%%|*}"
  deployed_mode="${deployed_mode:-static}"
  [[ "$deployed_mode" == "$requested_mode" ]] && exit 0
  {
    printf 'ERROR: kind cluster %s was created with SPACE_KIND_STORAGE_CREDENTIALS=%s,\n' \
      "$KIND_CLUSTER" "$deployed_mode"
    printf '       and this bring-up asks for %s. The two modes run different object\n' "$requested_mode"
    printf '       stores over different data directories behind object-store:8333, so\n'
    printf "       the cluster's suspend snapshots and published templates would vanish\n"
    printf '       from the locations its database records. Bring it up with\n'
    printf '       SPACE_KIND_STORAGE_CREDENTIALS=%s, or recreate it in the new mode; this\n' "$deployed_mode"
    printf '       deletes its sandboxes, snapshots and templates, and keeps the image caches:\n'
    printf '         %sbash infra/space/kind/scripts/teardown.sh\n' "$env_prefix"
    printf '         %sSPACE_KIND_STORAGE_CREDENTIALS=%s bash infra/space/kind/scripts/bootstrap.sh\n' \
      "$env_prefix" "$requested_mode"
    printf '       Nothing was changed. See "Object-store credentials" in\n'
    printf '       infra/space/kind/README.md.\n'
  } >&2
  exit 1
fi

{
  printf 'ERROR: kind cluster %s still runs the SeaweedFS object store, which this\n' "$KIND_CLUSTER"
  printf '       checkout replaced with versitygw (Service object-store:8333). Found in\n'
  printf '       namespace %s:\n' "$NAMESPACE"
  while IFS= read -r resource; do printf '         %s\n' "$resource"; done <<<"$legacy"
  printf '       Its suspend snapshots and published templates live only in SeaweedFS,\n'
  printf '       and the database records their locations as http://seaweedfs:8333, so\n'
  printf '       neither can move to the new store. Recreate the cluster; this deletes\n'
  printf '       its sandboxes, snapshots and templates, and keeps the image caches:\n'
  printf '         %sbash infra/space/kind/scripts/teardown.sh\n' "$env_prefix"
  printf '         %sbash infra/space/kind/scripts/bootstrap.sh\n' "$env_prefix"
  printf '       (or re-run the smoke suite, which creates the cluster itself; Tilt\n'
  printf '       users then run "tilt up"). Nothing was changed. See "Object store\n'
  printf '       transition" in infra/space/kind/README.md.\n'
} >&2
exit 1
