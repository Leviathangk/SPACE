# shellcheck shell=bash
# The kind smoke suites' own prerequisites, and where each one comes from.
#
# These are prerequisites of the suites rather than of the build or the node
# services, and their absence is expensive in two different ways: without lsof
# the shared bootstrap refuses to start a destructive smoke because it cannot
# identify the owner of the localhost API port, and without aws the node-drain
# suite cannot inspect the evacuation's snapshot artifacts in the object store.
#
# `infra/space/kind/scripts/install-prereqs.sh` provisions this list and
# `scripts/tests/lib/bootstrap-kind-stack.sh` verifies it before the suites
# run. Keep installation and verification on this shared contract.
#
# Sourced, never executed. Callers need nothing set up first.

# shellcheck source=scripts/lib/verified-download.sh
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/verified-download.sh"

# Prerequisites apt carries, named by command. The package name matches the
# command for each of these; a prerequisite whose package differs would need
# that mapping reintroduced here rather than assumed at the call sites.
SPACE_SMOKE_PREREQ_APT=(jq lsof)

# The AWS CLI comes from Amazon's bundled installer, not from Debian's `awscli`
# package, because on the devbox image that package cannot be configured at all.
# The image builds some packages through an apt layer that bypasses dpkg, and one
# of them (lcov) left `/usr/share/python3/runtime.d/lcov.rtupdate` behind while
# dpkg has no record of the package. Configuring `python3` runs every rtupdate
# hook; that one calls `py3clean lcov`, which asks dpkg for a file list that does
# not exist, fails with `cannot get content of lcov`, and leaves `python3` in
# `iF`. Every package depending on `python3:any` — `awscli` among them — then
# unpacks but never configures, and `apt-get install` exits non-zero. The
# bundled installer carries its own runtime and depends on no Debian python3,
# so it sidesteps the whole chain rather than waiting on an image fix.
SPACE_SMOKE_PREREQ_BUNDLED=(aws)

# Packages provisioning needs in order to deliver the prerequisites above,
# rather than tools the suites call: `unzip` unpacks the AWS CLI bundle. Both
# provisioning paths install these. Nothing asserts them as prerequisites,
# deliberately — `aws` present with `unzip` absent is a perfectly good box, and
# `space_smoke_install_aws_cli` checks what it needs at the point of use.
#
# `unzip` rather than the `python3 -m zipfile -e` idiom this repo uses for the
# protoc release: that loses file modes, and the bundle it would unpack is a tree
# of executables the installer then copies out, `aws/install` included.
SPACE_SMOKE_PREREQ_SUPPORT=(unzip)

# Pinned, like every other tool this repo downloads: two devboxes built a week
# apart otherwise run different AWS CLIs, and a bad upstream day has nothing to
# roll back to. Bump deliberately. Amazon keeps versioned URLs indefinitely.
SPACE_AWS_CLI_VERSION="${SPACE_AWS_CLI_VERSION:-2.36.25}"

# Where the bundle lands. The defaults need root; a caller without it points
# these at a directory it owns, which is what keeps the kind bring-up usable on
# a sudo-less host where every other tool is installed user-locally too.
SPACE_AWS_CLI_BIN_DIR="${SPACE_AWS_CLI_BIN_DIR:-/usr/local/bin}"
SPACE_AWS_CLI_INSTALL_DIR="${SPACE_AWS_CLI_INSTALL_DIR:-/usr/local/aws-cli}"

# Commands the suites need, whatever provides them.
space_smoke_prereq_commands() {
  # `${a[@]+…}` so an emptied bucket is an empty list rather than an unbound-variable
  # error under `set -u`.
  printf '%s\n' ${SPACE_SMOKE_PREREQ_APT[@]+"${SPACE_SMOKE_PREREQ_APT[@]}"} \
    ${SPACE_SMOKE_PREREQ_BUNDLED[@]+"${SPACE_SMOKE_PREREQ_BUNDLED[@]}"}
}

# Every apt package either provisioning path installs from this list.
space_smoke_prereq_packages() {
  printf '%s\n' ${SPACE_SMOKE_PREREQ_APT[@]+"${SPACE_SMOKE_PREREQ_APT[@]}"} \
    ${SPACE_SMOKE_PREREQ_SUPPORT[@]+"${SPACE_SMOKE_PREREQ_SUPPORT[@]}"}
}

space_smoke_prereq_is_apt_carried() {
  local name
  for name in ${SPACE_SMOKE_PREREQ_APT[@]+"${SPACE_SMOKE_PREREQ_APT[@]}"} \
    ${SPACE_SMOKE_PREREQ_SUPPORT[@]+"${SPACE_SMOKE_PREREQ_SUPPORT[@]}"}; do
    [[ "$1" == "$name" ]] && return 0
  done
  return 1
}

# Every prerequisite command that does not resolve. Support packages are not
# included: their absence matters only while installing, and reporting one as a
# missing prerequisite would fail a box that already has everything the suites
# use.
space_smoke_prereqs_missing() {
  local cmd
  while IFS= read -r cmd; do
    command -v "$cmd" >/dev/null 2>&1 || printf '%s\n' "$cmd"
  done < <(space_smoke_prereq_commands)
}

# The missing set as `command (where it comes from)` lines, which is what every
# caller wants to print.
space_smoke_prereqs_missing_report() {
  local cmd
  while IFS= read -r cmd; do
    if space_smoke_prereq_is_apt_carried "$cmd"; then
      printf '%s (apt package %s)\n' "$cmd" "$cmd"
    else
      printf '%s (AWS CLI v2 bundled installer)\n' "$cmd"
    fi
  done < <(space_smoke_prereqs_missing)
}

# Whether a given `aws` executable is the pinned bundle. The reported version is
# parsed and compared whole rather than substring-matched: a pin of `2.36.2`
# would otherwise be satisfied by `2.36.25`, and a rollback would silently skip.
space_smoke_aws_cli_at_pin() {
  [[ -x "$1" ]] || return 1
  local reported
  reported="$("$1" --version 2>&1 | sed -n 's|.*aws-cli/\([^[:space:]]*\).*|\1|p' | head -1)"
  [[ -n "$reported" && "$reported" == "$SPACE_AWS_CLI_VERSION" ]]
}

# Whether the `aws` on PATH is the pinned bundle. Presence alone is not enough:
# the devbox image's unconfigurable `awscli` deb leaves a working-looking
# `/usr/bin/aws` behind, and accepting it would mean provisioning silently keeps
# an unsupported install forever — the exact state this change exists to replace.
space_smoke_aws_cli_matches_pin() {
  local resolved
  resolved="$(command -v aws 2>/dev/null)" || return 1
  space_smoke_aws_cli_at_pin "$resolved"
}

# Install the AWS CLI v2 from Amazon's installer. Idempotent: returns
# immediately when the pinned version is already on PATH. Any arguments are used
# as the privileged-command prefix (`sudo`, `sudo -n`, or nothing when already
# root), because the two callers escalate differently.
space_smoke_install_aws_cli() {
  local privileged=("$@")
  # Keyed on the destination, not on PATH: a caller pointing at a directory that
  # has no `aws` needs the install to happen even when some other pinned copy is
  # reachable, or that directory silently ends up without one.
  if space_smoke_aws_cli_at_pin "$SPACE_AWS_CLI_BIN_DIR/aws"; then
    return 0
  fi
  if command -v aws >/dev/null 2>&1; then
    printf 'replacing %s (%s) with the pinned AWS CLI v2 %s in %s\n' \
      "$(command -v aws)" "$(aws --version 2>&1 | head -1)" \
      "$SPACE_AWS_CLI_VERSION" "$SPACE_AWS_CLI_BIN_DIR"
  fi
  local bin
  for bin in curl unzip; do
    if ! command -v "$bin" >/dev/null 2>&1; then
      printf 'ERROR: %s is required to install the AWS CLI v2 bundle\n' "$bin" >&2
      return 1
    fi
  done

  local arch
  arch="$(uname -m)"
  case "$arch" in
    x86_64 | aarch64) ;;
    *)
      # Linux-only by construction: the URL is the linux bundle. Callers on
      # other platforms skip this before reaching it.
      printf 'ERROR: the linux AWS CLI v2 bundle covers x86_64 and aarch64, not %s\n' \
        "$arch" >&2
      return 1
      ;;
  esac

  printf 'installing AWS CLI v2 %s (%s) into %s\n' \
    "$SPACE_AWS_CLI_VERSION" "$arch" "$SPACE_AWS_CLI_BIN_DIR"
  # Download and unpack in a subshell with an EXIT trap: the unpacked bundle is a
  # few hundred MB the installer copies out of, nothing downstream reads it, and
  # an EXIT trap in a subshell cleans up on every path without disturbing any
  # trap the caller has set.
  (
    set -e
    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT
    space_download_verified \
      "https://awscli.amazonaws.com/awscli-exe-linux-${arch}-${SPACE_AWS_CLI_VERSION}.zip" \
      "$tmp/awscliv2.zip" || exit 1
    unzip -q "$tmp/awscliv2.zip" -d "$tmp"
    # `--update` so a box that already has a bundle installed somewhere PATH does
    # not reach is repaired rather than refused.
    "${privileged[@]}" "$tmp/aws/install" \
      --bin-dir "$SPACE_AWS_CLI_BIN_DIR" --install-dir "$SPACE_AWS_CLI_INSTALL_DIR" \
      --update >/dev/null
  ) || return 1

  # Verified where it was installed, not through PATH: on a first sudo-less run
  # the target directory legitimately is not on PATH yet, and the caller's job
  # (`ensure_path` in the kind script) is to fix that afterwards. Failing here
  # would break that flow over a successful install.
  if ! space_smoke_aws_cli_at_pin "$SPACE_AWS_CLI_BIN_DIR/aws"; then
    printf 'ERROR: the installer reported success but %s/aws is not the pinned %s\n' \
      "$SPACE_AWS_CLI_BIN_DIR" "$SPACE_AWS_CLI_VERSION" >&2
    return 1
  fi
  if ! command -v aws >/dev/null 2>&1; then
    printf 'note: installed %s/aws, which is not on PATH yet — add that directory\n' \
      "$SPACE_AWS_CLI_BIN_DIR"
  fi
  return 0
}

# Kind shares the host kernel. Containers cannot supply missing target modules
# merely by installing dmsetup, and a Devbox pod cannot load them at all. Only
# the BYOK smoke needs them, so the Kind bring-up does not require them: Kind CI
# loads them explicitly, and the scenario's preflight reports them missing on a
# host that has neither.
space_smoke_encrypted_targets_ready() {
  if command -v dmsetup >/dev/null 2>&1; then
    local targets
    targets="$("$@" dmsetup targets 2>/dev/null)" || return 1
    grep -q '^crypt[[:space:]]' <<<"$targets" &&
      grep -q '^snapshot[[:space:]]' <<<"$targets"
  else
    [[ -d /sys/module/dm_crypt && -d /sys/module/dm_snapshot ]]
  fi
}

space_smoke_prepare_encrypted_targets() {
  local host_root=() module
  if (( EUID != 0 )); then
    host_root=(sudo -n)
  fi
  if ! space_smoke_encrypted_targets_ready "${host_root[@]}"; then
    if ! command -v modprobe >/dev/null 2>&1; then
      echo "ERROR: BYOK needs modprobe (kmod package) and modules matching host kernel $(uname -r)" >&2
      return 1
    fi
    for module in dm_crypt dm_snapshot; do
      if ! "${host_root[@]}" modprobe "$module"; then
        echo "ERROR: cannot load $module for host kernel $(uname -r); containerized hosts must expose matching modules or load them on the host" >&2
        return 1
      fi
    done
    if ! space_smoke_encrypted_targets_ready "${host_root[@]}"; then
      echo "ERROR: host device-mapper crypt/snapshot targets remain unavailable" >&2
      return 1
    fi
  fi
  echo "[byok] host device-mapper crypt and snapshot targets ready"
}
