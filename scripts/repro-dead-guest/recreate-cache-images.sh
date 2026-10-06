#!/bin/bash
# recreate-cache-images.sh — LOCAL KIND ONLY. Start the sandbox workers on fresh
# btrfs cache images, the chart's documented clean start ("delete the file on
# the kind node", values.yaml cache.persistImage). Requires no sandbox VMs on
# the workers. Templates are re-pulled by template-manager.
set -euo pipefail
export PATH=$HOME/.local/bin:$PATH
K=(kubectl --context kind-space-local -n space)
for n in space-local-worker space-local-worker2; do
  if docker exec "$n" pgrep -f '^/firecracker --id' >/dev/null; then
    echo "refusing: Firecracker processes still running on $n" >&2; exit 1
  fi
done
"${K[@]}" patch ds space-node-stack --type=json \
  -p '[{"op":"add","path":"/spec/template/spec/nodeSelector/repro-paused","value":"true"}]'
"${K[@]}" wait --for=delete pod -l app.kubernetes.io/name=space-worker --timeout=300s || true
sleep 3
for n in space-local-worker space-local-worker2; do
  docker exec "$n" sh -c 'findmnt -n /space/cache && { echo "still mounted"; exit 1; }; rm -f /var/lib/space-cache.img; echo "removed cache image on $(hostname)"'
done
"${K[@]}" patch ds space-node-stack --type=json \
  -p '[{"op":"remove","path":"/spec/template/spec/nodeSelector/repro-paused"}]'
"${K[@]}" rollout status ds/space-node-stack --timeout=600s
