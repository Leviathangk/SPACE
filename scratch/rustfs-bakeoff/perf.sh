#!/usr/bin/env bash
# perf.sh — criterion D for one store: image size, start → ready, idle and
# peak memory and CPU, first write to a new bucket, and the upload burst
# (probe `burst`, 3 runs of 36 x 128 MiB) with zero-progress sampling on the
# store's veth. Prints a Markdown table; raw samples go to $OUT.
#
# Usage: perf.sh <rustfs|versitygw>
# Env:   PROBE (default: the release build under $CARGO_TARGET_DIR),
#        OUT (default /tmp/rustfs-bakeoff-perf), RUNS (3), STARTS (3)
set -euo pipefail

store="${1:?store}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROBE="${PROBE:-${CARGO_TARGET_DIR:-$here/probe/target}/release/rustfs-bakeoff-probe}"
OUT="${OUT:-/tmp/rustfs-bakeoff-perf}"
RUNS="${RUNS:-3}"
STARTS="${STARTS:-3}"
mkdir -p "$OUT"
name="bakeoff-$store"
case "$store" in
  rustfs) image="$(sed -n 's/^RUSTFS_IMAGE="${RUSTFS_IMAGE:-\(.*\)}"$/\1/p' "$here/stores.sh")" ;;
  versitygw) image="$(sed -n 's/^VERSITYGW_IMAGE="${VERSITYGW_IMAGE:-\(.*\)}"$/\1/p' "$here/stores.sh")" ;;
  *) echo "unknown store $store" >&2; exit 2 ;;
esac

docker pull -q "$image" >/dev/null
unpacked=$(docker image inspect -f '{{.Size}}' "$image")
# Compressed: the linux/amd64 manifest's layer sizes, as a registry serves them.
manifest_digest=$(docker buildx imagetools inspect --raw "$image" \
  | jq -r 'if .manifests then (.manifests[] | select(.platform.architecture == "amd64" and .platform.os == "linux") | .digest) else empty end' | head -1)
repo="${image%@*}"; repo="${repo%:*}"
if [[ -n "$manifest_digest" ]]; then
  compressed=$(docker buildx imagetools inspect --raw "$repo@$manifest_digest" | jq '[.layers[].size] | add')
else
  compressed=$(docker buildx imagetools inspect --raw "$image" | jq '[.layers[].size] | add')
fi

# start → ready on a fresh data volume, STARTS times, then one restart on data.
starts=()
for _ in $(seq "$STARTS"); do
  starts+=("$("$here/stores.sh" up "$store")")
done
eval "$("$here/stores.sh" env "$store")"
"$PROBE" --label "$store" first-write --repeats 5 >"$OUT/$store-first-write.md" || true
restart="$("$here/stores.sh" restart "$store")"
eval "$("$here/stores.sh" env "$store")"

# Idle: 30 s after start, ten one-second samples.
sleep 30
docker stats --no-stream --format '{{json .}}' "$name" >/dev/null
: >"$OUT/$store-idle.jsonl"
for _ in $(seq 10); do
  docker stats --no-stream --format '{{json .}}' "$name" >>"$OUT/$store-idle.jsonl"
done

# Bursts: each run on a fresh store, so space a store frees lazily after
# deletes (RustFS empties its trash every five minutes) cannot carry over,
# with docker stats streaming for the peak.
: >"$OUT/$store-burst.md"
: >"$OUT/$store-burst-stats.jsonl"
: >"$OUT/$store-burst.err"
for run in $(seq "$RUNS"); do
  "$here/stores.sh" up "$store" >/dev/null
  eval "$("$here/stores.sh" env "$store")"
  veth="$("$here/stores.sh" veth "$store")"
  docker stats --format '{{json .}}' "$name" >>"$OUT/$store-burst-stats.jsonl" &
  stats_pid=$!
  "$PROBE" --label "$store run $run" burst --runs 1 --rx-counter "/sys/class/net/$veth/statistics/tx_bytes" \
    2>>"$OUT/$store-burst.err" >>"$OUT/$store-burst.md" || true   # a failed check is still reported
  kill "$stats_pid" 2>/dev/null || true
  wait "$stats_pid" 2>/dev/null || true
done

python3 - "$store" "$image" "$compressed" "$unpacked" "$OUT" "$restart" "${starts[@]}" <<'PY' | tee "$OUT/$store-perf.md"
import json, re, statistics, sys
store, image, compressed, unpacked, out, restart, *starts = sys.argv[1:]
def samples(path):
    text = open(path, errors="replace").read()
    return [json.loads(m) for m in re.findall(r'\{"BlockIO[^}]*\}', text)]
def mib(s):
    v = s["MemUsage"].split("/")[0].strip()
    n = float(re.sub(r"[A-Za-z]", "", v)); u = re.sub(r"[0-9.]", "", v)
    return n * {"GiB": 1024, "MiB": 1, "KiB": 1 / 1024, "B": 1 / 1048576}.get(u, 1)
def cpu(s):
    return float(s["CPUPerc"].rstrip("%"))
def med_max(vals):
    return f"median {statistics.median(vals):.1f}, max {max(vals):.1f}" if vals else "n/a"
idle = samples(f"{out}/{store}-idle.jsonl")
burst = samples(f"{out}/{store}-burst-stats.jsonl")
runs = [dict(re.findall(r"(\w+)=(\S+)", l)) for l in open(f"{out}/{store}-burst.md") if l.startswith("BURST_SUMMARY")]
def mw(key):
    vals = sorted(float(r[key]) for r in runs)
    return f"{statistics.median(vals):.2f} s / {vals[-1]:.2f} s" if vals else "n/a"
def ready(line):
    m = re.search(r"health_s=([0-9.]+) s3_s=([0-9.]+)", line)
    return f"health {m.group(1)} s, first S3 answer {m.group(2)} s" if m else line
fw = re.search(r"median_ms=([0-9.]+)", open(f"{out}/{store}-first-write.md").read())
print(f"### D. Start-up and performance — {store}\n")
print(f"Image `{image}`: {int(compressed) / 1e6:.0f} MB compressed (linux/amd64 layers), {int(unpacked) / 1e6:.0f} MB unpacked.\n")
print("| Measure | Result |\n|---|---|")
for i, s in enumerate(starts, 1):
    print(f"| start → ready, fresh data, run {i} | {ready(s)} |")
print(f"| start → ready, restart on data | {ready(restart)} |")
print(f"| first write to a new bucket | median {fw.group(1) if fw else '?'} ms over 5 new buckets |")
print(f"| idle memory (MiB) | {med_max([mib(s) for s in idle])} |")
print(f"| idle CPU (% of one core) | {med_max([cpu(s) for s in idle])} |")
print(f"| memory during the bursts (MiB) | {med_max([mib(s) for s in burst])} |")
print(f"| CPU during the bursts (% of one core) | {med_max([cpu(s) for s in burst])} |")
print(f"| burst phase (client put_file), median / worst of {len(runs)} | {mw('phase_median_s')} |")
print(f"| slowest PUT, median / worst | {mw('slowest_put_median_s')} |")
print(f"| longest ACK gap (body sent → response), median / worst | {mw('ack_gap_median_s')} |")
print(f"| zero-progress stalls >= 5 s while a body is sending | {sum(int(r['stalls']) for r in runs)} |")
print(f"| failed uploads | {sum(int(r['failures']) for r in runs)} of {len(runs) * 72} |")
print()
for l in open(f"{out}/{store}-burst.md"):
    if l.startswith("| D") and not l.startswith("| Dm"):
        print(l, end="")
PY
