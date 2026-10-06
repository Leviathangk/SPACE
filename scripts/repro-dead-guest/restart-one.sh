#!/bin/bash
# restart-one.sh NAME — restart ONE supervised node-stack service in place
# (run as root inside the node-stack container), leaving every other service
# running. The entrypoint's supervise_services exits the whole container when
# any pid in /run/space-node-stack/*.pid dies, so the pid file points at a
# placeholder while the service is stopped (SIGTERM, as in a pod shutdown) and
# re-executed with its original argv, environment, uid/gid/groups, cwd and log.
set -euo pipefail
name=${1:?service name, e.g. sbxmgr or nodemgr}
pidf=/run/space-node-stack/$name.pid
pid=$(cat "$pidf")
mapfile -d '' argv < "/proc/$pid/cmdline"
mapfile -d '' envv < "/proc/$pid/environ"
uid=$(awk '/^Uid:/{print $2}' "/proc/$pid/status")
gid=$(awk '/^Gid:/{print $2}' "/proc/$pid/status")
groups=$(awk '/^Groups:/{$1=""; print}' "/proc/$pid/status" | xargs | tr ' ' ',')
cwd=$(readlink "/proc/$pid/cwd")
log=$(readlink "/proc/$pid/fd/1")
setsid sleep infinity </dev/null >/dev/null 2>&1 &
placeholder=$!
echo "$placeholder" > "$pidf"
kill -TERM "$pid"
for _ in $(seq 1 120); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
kill -0 "$pid" 2>/dev/null && kill -KILL "$pid"
stopped=$(date -u +%FT%T.%3NZ)
echo "[restart-one] $stopped $name pid $pid stopped; re-executing" >> "$log"
cd "$cwd"
pre=()
[[ "$uid" == 0 ]] || pre=(setpriv --reuid "$uid" --regid "$gid" --groups "$groups" --)
setsid env -i "${envv[@]}" "${pre[@]}" "${argv[@]}" </dev/null >>"$log" 2>&1 &
new=$!
echo "$new" > "$pidf"
kill "$placeholder"
echo "restarted $name: old=$pid stopped=$stopped new=$new"
