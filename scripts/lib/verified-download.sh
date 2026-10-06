# shellcheck shell=bash
# Pins are reviewed with the installer versions; never trust a downloaded checksum.
SPACE_DOWNLOAD_CHECKSUMS="${SPACE_DOWNLOAD_CHECKSUMS:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/download-checksums.txt}"

space_download_verified() (
  set -euo pipefail
  local url="$1" destination="$2" expected actual temporary
  local hash=(sha256sum)
  [[ "$url" == https://* ]] || { echo 'Download requires HTTPS' >&2; return 1; }
  expected="$(awk -v url="$url" '$2 == url { print (NF == 2 ? $1 : "invalid") }' "$SPACE_DOWNLOAD_CHECKSUMS")" || return 1
  if [[ ! "$expected" =~ ^[a-f0-9]{64}$ ]]; then
    printf 'Expected exactly one SHA256 pin for %s in %s\n' "$url" "$SPACE_DOWNLOAD_CHECKSUMS" >&2
    return 1
  fi
  if ! command -v sha256sum >/dev/null 2>&1; then
    hash=(shasum -a 256)
    command -v shasum >/dev/null 2>&1 || { echo 'Install sha256sum or shasum' >&2; return 1; }
  fi
  temporary="$(mktemp "${destination}.download.XXXXXX")" || return 1
  trap 'rm -f "$temporary"' EXIT
  curl -fsSL --proto '=https' --proto-redir '=https' --retry 6 --retry-all-errors \
    "$url" -o "$temporary" || return 1
  actual="$("${hash[@]}" "$temporary" | awk '{print $1}')" || return 1
  if [[ "$actual" != "$expected" ]]; then
    printf 'SHA256 mismatch for %s; destination left unchanged\n' "$url" >&2
    return 1
  fi
  chmod 0644 "$temporary" || return 1
  mv -f "$temporary" "$destination"
)
