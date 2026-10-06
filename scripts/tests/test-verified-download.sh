#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
export SPACE_DOWNLOAD_CHECKSUMS="$scratch/checksums"
# shellcheck source=scripts/lib/verified-download.sh
source "$repo_root/scripts/lib/verified-download.sh"

# A transport fixture, with no network or executable download.
curl() {
  touch "$scratch/requested"
  printf '%s' "$payload" > "${@: -1}"
  return "$transport_status"
}

# SHA256 of the standard "abc" test vector.
digest=ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
for scenario in valid mismatch missing duplicate malformed http transport; do
  payload=abc
  transport_status=0
  url=https://download.example.test/tool
  printf '%s  %s\n' "$digest" "$url" > "$SPACE_DOWNLOAD_CHECKSUMS"
  printf 'previous installation' > "$scratch/tool"
  rm -f "$scratch/requested"
  case "$scenario" in
    mismatch) payload=corrupt ;;
    missing) : > "$SPACE_DOWNLOAD_CHECKSUMS" ;;
    duplicate) printf '%s  %s\n' "$digest" "$url" >> "$SPACE_DOWNLOAD_CHECKSUMS" ;;
    malformed) printf 'not-a-digest  %s\n' "$url" > "$SPACE_DOWNLOAD_CHECKSUMS" ;;
    http) url=http://download.example.test/tool ;;
    transport) transport_status=22 ;;
  esac
  if space_download_verified "$url" "$scratch/tool" 2> "$scratch/error"; then
    [[ "$scenario" == valid && "$(cat "$scratch/tool")" == abc ]]
  else
    [[ "$scenario" != valid && "$(cat "$scratch/tool")" == 'previous installation' ]]
  fi
  case "$scenario" in
    missing|duplicate|malformed|http) [[ ! -e "$scratch/requested" ]] ;;
  esac
  for temporary in "$scratch"/*.download.*; do
    [[ ! -e "$temporary" ]]
  done
  printf 'PASS: %s\n' "$scenario"
done
