#!/usr/bin/env bash
# Run the portable typed smoke inventory against the local Kind/Firecracker stack.
# Usage: scripts/tests/smoke-kind-typed.sh [all|ecw|network_policy|local|native] [INDEX/COUNT]
# Each shard needs its own isolated Kind stack; scenarios within it stay serial.
#   `all` is the full inventory: every scenario on the ECW API surface, then every scenario
#   that also supports the native /v1 surface on that surface. The two passes are separate
#   runner processes because the surface is process-wide; `all` runs both, repeats both
#   passes' summaries (counts and every scenario that did not pass) at the end, and exits
#   with the first nonzero status. `native` runs only the second pass. Both need
#   SPACE_API_TOKEN (an admin session bearer token) or SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE to
#   read one from.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TARGET="${1:-ecw}"
SHARD_ARGS=()
if (( $# > 2 )); then
  echo "ERROR: expected a target and optional shard INDEX/COUNT" >&2
  exit 2
fi
if [[ -n "${2:-}" ]]; then
  SHARD_ARGS=(--shard "$2")
fi

if [[ ! -c /dev/kvm || ! -r /dev/kvm || ! -w /dev/kvm ]]; then
  echo "ERROR: the typed Kind smoke requires readable and writable /dev/kvm" >&2
  echo "       Run it on a Linux KVM devbox, not a laptop-only Kind cluster." >&2
  exit 2
fi
if ! command -v uv >/dev/null 2>&1; then
  echo "ERROR: uv is required; run infra/space/kind/scripts/install-prereqs.sh" >&2
  exit 2
fi

# As in ci/scripts/run-python-tests.sh: CI checks that uv.lock still matches
# pyproject.toml (--locked); local runs use the lock as committed (--frozen),
# which a host uv with its own resolver settings would otherwise rewrite.
UV_LOCK_MODE=--frozen
if [[ -n "${CI:-}" ]]; then
  UV_LOCK_MODE=--locked
fi

# shellcheck source=lib/typed-kind-fixture.sh
. "$SCRIPT_DIR/lib/typed-kind-fixture.sh"

run_surface() {
  local surface="$1"; shift
  echo "--- typed smoke on the ${surface} API surface"
  SPACE_SMOKE_API_SURFACE="$surface" uv run "$UV_LOCK_MODE" --python 3.11 -m space_smoke run "$@" "${SHARD_ARGS[@]}"
}

cd "$REPO_ROOT"
export SPACE_SMOKE_BACKEND=kind

case "$TARGET" in
  all)
    # Check the native prerequisites before the long ECW pass so a missing token
    # fails in seconds, not after the whole inventory has run.
    prepare_native_kind
    ensure_build_foreign_org
    logs="$(mktemp -d)"
    trap 'rm -rf "$logs"' EXIT
    ecw_rc=0
    native_rc=0
    run_surface ecw --all 2>&1 | tee "$logs/ecw" || ecw_rc=$?
    run_surface native --all 2>&1 | tee "$logs/native" || native_rc=$?
    # Each pass ends on its SUITE count and a NOT PASSED block, but by now the ECW
    # pass's is above the whole native pass. Repeat both where the log ends, minus
    # the GitHub annotation lines, which would otherwise be raised twice. A pass
    # that stopped before running anything has no SUITE line; show its last lines.
    echo
    echo "=== typed smoke summary: ecw exit ${ecw_rc}, native exit ${native_rc} ==="
    for surface in ecw native; do
      echo "--- ${surface} API surface"
      if grep -q '^SUITE: ' "$logs/$surface"; then
        sed -n '/^SUITE: /,$p' "$logs/$surface" | grep -v '^::' || true
      else
        tail -n 20 "$logs/$surface"
      fi
    done
    if [[ "$ecw_rc" -ne 0 ]]; then
      exit "$ecw_rc"
    fi
    exit "$native_rc"
    ;;
  native)
    prepare_native_kind
    exec env SPACE_SMOKE_API_SURFACE=native uv run "$UV_LOCK_MODE" --python 3.11 -m space_smoke run --all "${SHARD_ARGS[@]}"
    ;;
  ecw|network_policy|local)
    if [[ "$TARGET" == local ]]; then
      ensure_build_foreign_org
    fi
    exec env SPACE_SMOKE_API_SURFACE=ecw uv run "$UV_LOCK_MODE" --python 3.11 -m space_smoke run --category "$TARGET" "${SHARD_ARGS[@]}"
    ;;
  *)
    echo "ERROR: unknown typed smoke target: $TARGET" >&2
    echo "       use all, ecw, network_policy, local, or native" >&2
    exit 2
    ;;
esac
