#!/bin/bash
# probe-loop.sh IDS_FILE OUT_JSONL [INTERVAL] — Process.Start reachability sample
# (`driver.py probe`) for every sandbox id listed in IDS_FILE (re-read each round).
ids=${1:?ids file}; out=${2:?out}; every=${3:-60}
here=$(cd "$(dirname "$0")" && pwd)
while :; do
  timeout 300 python3 "$here/driver.py" probe $(grep -v '^#' "$ids") >> "$out" 2>&1
  sleep "$every"
done
