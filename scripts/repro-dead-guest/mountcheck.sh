#!/bin/bash
# mountcheck.sh — for every VM on each sandbox worker, compare the guest vsock
# UDS identity as the node-stack pod resolves the jail path vs. through the
# Firecracker process's own root; also list the cache loop devices and the
# pod's /space/cache mount. Identical dev:ino = same socket the VM listens on.
export PATH=$HOME/.local/bin:$PATH
K="kubectl --context kind-space-local -n space"
for p in $($K get pods -l app.kubernetes.io/name=space-worker -o name | sed 's#pod/##'); do
  n=$($K get pod $p -o jsonpath='{.spec.nodeName}')
  echo "## $(date -u +%FT%TZ) pod=$p node=$n"
  $K exec $p -c node-stack -- sh -c 'grep " /space/cache " /proc/self/mountinfo | cut -d" " -f3-10; for d in /space/cache/firecracker/jails/firecracker/*/; do id=$(basename $d); printf "pod-view %s %s\n" $id "$(stat -c "%d:%i" $d/root/v.sock 2>&1)"; done'
  docker exec $n sh -c 'for pid in $(pgrep -f "^/firecracker --id"); do id=$(tr "\0" " " < /proc/$pid/cmdline | awk "{print \$3}"); printf "fc-root  %s %s pid=%s\n" $id "$(stat -L -c "%d:%i" /proc/$pid/root/v.sock 2>&1)" $pid; done; losetup -a 2>/dev/null | grep -i cache'
done
