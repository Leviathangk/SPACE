#!/bin/bash
# reset-cache-mounts.sh — LOCAL KIND ONLY. Return the sandbox workers to main's
# chart with exactly one btrfs instance per cache image, after
# cache-mount-patch.sh and restart experiments left several instances of the
# same image mounted (one per container generation that still had live jails).
#
# Precondition: every sandbox on the workers is deleted (no /firecracker
# processes), so no jail mount namespace still pins an old instance.
# Steps: take the worker node-stack pods off the nodes (temporary nodeSelector),
# unmount the node-namespace /space/cache left by the hostPath patch while
# nothing uses it, then revert the cache volume and drop the selector in one
# patch, and print the loop devices / mounts per node.
set -euo pipefail
export PATH=$HOME/.local/bin:$PATH
K=(kubectl --context kind-space-local -n space)
here=$(cd "$(dirname "$0")" && pwd)
for n in space-local-worker space-local-worker2; do
  if docker exec "$n" pgrep -f '^/firecracker --id' >/dev/null; then
    echo "refusing: Firecracker processes still running on $n" >&2; exit 1
  fi
done
"${K[@]}" patch ds space-node-stack --type=json \
  -p '[{"op":"add","path":"/spec/template/spec/nodeSelector/repro-paused","value":"true"}]'
"${K[@]}" wait --for=delete pod -l app.kubernetes.io/name=space-worker --timeout=300s || true
for n in space-local-worker space-local-worker2; do
  if docker exec "$n" findmnt -n /space/cache >/dev/null 2>&1; then
    docker exec "$n" umount /space/cache && echo "$n: unmounted node-namespace /space/cache"
  fi
done
sleep 2
for n in space-local-worker space-local-worker2; do
  echo "== $n loop devices for the cache image while no pod runs:"
  docker exec "$n" sh -c 'losetup -a | grep space-cache.img || echo none'
done
"$here/cache-mount-patch.sh" revert >/dev/null 2>&1 || true   # rollout status times out with 0 pods; the patch applies
"${K[@]}" patch ds space-node-stack --type=json \
  -p '[{"op":"remove","path":"/spec/template/spec/nodeSelector/repro-paused"}]'
"${K[@]}" rollout status ds/space-node-stack --timeout=600s
"$here/mountcheck.sh" | grep -E '^##|btrfs|loop'
