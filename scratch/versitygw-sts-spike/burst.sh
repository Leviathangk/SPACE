#!/usr/bin/env bash
# burst.sh — the bake-off's criterion-D upload burst (36 x 128 MiB at once,
# client put_file then SDK PutObject, zero-progress sampling on the gateway's
# veth) on the running stack, with three kinds of credentials, interleaved:
#
#   static   the root keys (no IAM service call per request: the bake-off's
#            and Kind's static mode)
#   session  a session from the bridge (the authority's client), scoped by
#            the authority's prefix policy, so every request is evaluated
#            against the role's policy and the session policy
#   user     static keys of an IAM user holding the vend policy (info: policy
#            evaluation without a session)
#
# Usage: burst.sh   (after stack.sh up)
# Env:   PROBE (default: $CARGO_TARGET_DIR/release/versitygw-sts-spike-probe),
#        OUT (default /tmp/versitygw-sts-spike-burst), RUNS (3), MODES
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE="${PROBE:-${CARGO_TARGET_DIR:-$here/probe/target}/release/versitygw-sts-spike-probe}"
OUT="${OUT:-/tmp/versitygw-sts-spike-burst}"
RUNS="${RUNS:-3}"
read -r -a MODES <<<"${MODES:-static session user}"
mkdir -p "$OUT"
eval "$("$here/stack.sh" env)"
export RUST_LOG="${RUST_LOG:-error}"

idx="$(docker run --rm --net container:vsts-gw busybox cat /sys/class/net/eth0/iflink)"
veth="$(ip -o link | awk -F': ' -v i="$idx" '$1 == i { sub("@.*", "", $2); print $2 }')"
[[ -n "$veth" ]] || { echo "no veth for vsts-gw" >&2; exit 1; }

iam() {
  AWS_ACCESS_KEY_ID=$BAKEOFF_ACCESS_KEY AWS_SECRET_ACCESS_KEY=$BAKEOFF_SECRET_KEY AWS_REGION=us-east-1 \
    aws --endpoint-url "$SPIKE_IAM_ENDPOINT" --output json iam "$@"
}
if [[ " ${MODES[*]} " == *" user "* ]]; then
  iam delete-user-policy --user-name spike-burst --policy-name vend >/dev/null 2>&1 || true
  for k in $(iam list-access-keys --user-name spike-burst 2>/dev/null | jq -r '.AccessKeyMetadata[].AccessKeyId'); do
    iam delete-access-key --user-name spike-burst --access-key-id "$k" >/dev/null
  done
  iam delete-user --user-name spike-burst >/dev/null 2>&1 || true
  iam create-user --user-name spike-burst >/dev/null
  iam put-user-policy --user-name spike-burst --policy-name vend --policy-document \
    '{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject","s3:AbortMultipartUpload"],"Resource":["arn:aws:s3:::snapshots/*","arn:aws:s3:::templates/*"]},{"Effect":"Allow","Action":["s3:ListBucket"],"Resource":["arn:aws:s3:::snapshots","arn:aws:s3:::templates"]}]}' >/dev/null
  user_key="$(iam create-access-key --user-name spike-burst)"
fi

: >"$OUT/burst.md"
: >"$OUT/stats.jsonl"
for run in $(seq "$RUNS"); do
  for mode in "${MODES[@]}"; do
    unset BAKEOFF_WRITER_ACCESS_KEY BAKEOFF_WRITER_SECRET_KEY BAKEOFF_WRITER_SESSION_TOKEN
    case "$mode" in
      static) ;;
      session) eval "$("$PROBE" session --sts-mode provider --prefix "spike/burst-session")" ;;
      user)
        export BAKEOFF_WRITER_ACCESS_KEY="$(jq -r .AccessKey.AccessKeyId <<<"$user_key")"
        export BAKEOFF_WRITER_SECRET_KEY="$(jq -r .AccessKey.SecretAccessKey <<<"$user_key")"
        ;;
    esac
    docker stats --format '{{json .}}' vsts-gw vsts-iam | sed -u "s/^/$mode $run /" >>"$OUT/stats.jsonl" &
    stats_pid=$!
    "$PROBE" --label "$mode run $run" burst --runs 1 --bucket snapshots --key-prefix "spike/burst-$mode" \
      --rx-counter "/sys/class/net/$veth/statistics/tx_bytes" >>"$OUT/burst.md" 2>>"$OUT/burst.err" || true
    kill "$stats_pid" 2>/dev/null || true
    wait "$stats_pid" 2>/dev/null || true
  done
done

python3 - "$OUT" <<'PY' | tee "$OUT/summary.md"
import json, re, statistics, sys
out = sys.argv[1]
runs = {}
for l in open(f"{out}/burst.md"):
    if l.startswith("BURST_SUMMARY"):
        d = dict(re.findall(r"(\w+)=(\S+)", l))
        mode = re.search(r"label=(\w+)", l).group(1)
        runs.setdefault(mode, []).append(d)
def mib(v):
    v = v.split("/")[0].strip(); n = float(re.sub(r"[A-Za-z]", "", v)); u = re.sub(r"[0-9.]", "", v)
    return n * {"GiB": 1024, "MiB": 1, "KiB": 1 / 1024, "B": 1 / 1048576}.get(u, 1)
stats = {}
for l in open(f"{out}/stats.jsonl", errors="replace"):
    m = re.match(r"(\w+) \d+ .*?(\{.*\})", l)
    if not m: continue
    try: s = json.loads(m.group(2))
    except ValueError: continue
    if s.get("CPUPerc", "--") in ("--", ""): continue
    stats.setdefault((m.group(1), s["Name"]), []).append((float(s["CPUPerc"].rstrip("%")), mib(s["MemUsage"])))
def med(mode, key):
    v = sorted(float(r[key]) for r in runs.get(mode, []))
    return statistics.median(v) if v else float("nan")
print("| Credentials | runs | client phase, median (s) | client phase, worst (s) | slowest PUT, median (s) | max ACK gap, median (s) | stalls >= 5 s | failed uploads | gateway CPU median / max (%) | IAM service CPU median / max (%) |")
print("|---|---|---|---|---|---|---|---|---|---|")
for mode in ("static", "session", "user"):
    if mode not in runs: continue
    r = runs[mode]
    def cpu(name):
        v = [c for c, _ in stats.get((mode, name), [])]
        return f"{statistics.median(v):.0f} / {max(v):.0f}" if v else "n/a"
    print(f"| {mode} | {len(r)} | {med(mode, 'phase_median_s'):.2f} | {max(float(x['phase_median_s']) for x in r):.2f} | {med(mode, 'slowest_put_median_s'):.2f} | {med(mode, 'ack_gap_median_s'):.2f} | {sum(int(x['stalls']) for x in r)} | {sum(int(x['failures']) for x in r)} | {cpu('vsts-gw')} | {cpu('vsts-iam')} |")
if "static" in runs and "session" in runs:
    print(f"\nsession / static, client phase medians: {med('session', 'phase_median_s') / med('static', 'phase_median_s'):.3f}")
PY
