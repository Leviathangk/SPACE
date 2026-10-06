#!/bin/bash
# capture.sh SBX_ID OUT_DIR — snapshot everything about one sandbox the moment
# the workload sees it fail: raw vsock CONNECT 5002 (guest alive vs host-side),
# Firecracker process state (threads, wchan), fc.log, Firecracker API instance
# state, sbxmgr health/doctor, nodemgr inspect, the last 30 min of every
# node-stack log line for the sandbox, and a guest dmesg attempt. Repeats the
# vsock + health sample every 60 s for 30 min afterwards.
sbx=${1:?sandbox id}; out=${2:?out dir}; uid=${sbx#sbx_}
here=$(cd "$(dirname "$0")" && pwd)
export PATH=$HOME/.local/bin:$PATH
K="kubectl --context kind-space-local -n space"
mkdir -p "$out"; exec > >(tee -a "$out/capture.log") 2>&1
echo "## capture $sbx at $(date -u +%FT%TZ)"
node=$($K get pods -l space.pplx.net/sandbox-id=$uid -o jsonpath='{.items[0].spec.nodeName}')
pod=$($K get pods -l app.kubernetes.io/name=space-worker -o wide | awk -v n="$node" '$7==n{print $1}')
echo "node=$node node_stack_pod=$pod"
fc=$(docker exec "$node" pgrep -f "^/firecracker --id $uid")
echo "firecracker pid=$fc"
docker exec -i "$node" python3 - "$node" < "$here/node_probe.py" | grep "$uid" > "$out/vsock-0.json"; cat "$out/vsock-0.json"
docker exec "$node" sh -c "cat /proc/$fc/status | grep -E 'State|VmRSS|Threads'; for t in /proc/$fc/task/*; do echo \"\$(basename \$t) \$(cat \$t/comm) \$(awk '{print \$3}' \$t/stat) wchan=\$(cat \$t/wchan)\"; done" > "$out/fc-proc.txt" 2>&1
docker exec "$node" sh -c "tail -n 3000 /proc/$fc/root/fc.log" > "$out/fc.log" 2>&1
docker exec "$node" sh -c "curl -s --max-time 3 --unix-socket /proc/$fc/root/firecracker.sock http://localhost/; echo; curl -s --max-time 3 --unix-socket /proc/$fc/root/firecracker.sock http://localhost/machine-config" > "$out/fc-api.txt" 2>&1
$K exec "$pod" -c node-stack -- sh -c "space-sbxmgr health $uid; echo ---; space-sbxmgr doctor $uid; echo ---; space-nodemgr inspect $uid" > "$out/sbxmgr-health-doctor.txt" 2>&1
since=$(date -u -d '-30 min' +%FT%T)
$K exec "$pod" -c node-stack -- sh -c "for f in /space/logs/*.log; do grep -h '$uid' \$f | awk -v s='$since' -F'\"timestamp\":\"' '{ if (NF<2 || substr(\$2,1,19) >= s) print }' > /tmp/cap-\$(basename \$f); done; tar -C /tmp -cf - \$(cd /tmp && ls cap-*.log)" | tar -C "$out" -xf - 2>/dev/null
timeout 120 python3 "$here/driver.py" exec "$sbx" --user root -- sh -c 'dmesg | tail -100; free -m; cat /proc/loadavg; ps -eo pid,stat,rss,etimes,args --sort=-rss | head -20' > "$out/guest-dmesg.txt" 2>&1
for i in $(seq 1 30); do
  sleep 60
  docker exec -i "$node" python3 - "$node" < "$here/node_probe.py" | grep "$uid" >> "$out/vsock-follow.jsonl"
  $K exec "$pod" -c node-stack -- space-sbxmgr health "$uid" 2>&1 | head -20 | sed "s/^/$(date -u +%T) /" >> "$out/health-follow.txt"
done
echo "## capture done $(date -u +%FT%TZ)"
