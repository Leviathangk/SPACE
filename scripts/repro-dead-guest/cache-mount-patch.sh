#!/bin/bash
# cache-mount-patch.sh apply|revert — LOCAL KIND EXPERIMENT ONLY.
# apply: give the *worker* space-node-stack DaemonSet the builder role's
#   /space/cache treatment (hostPath on the Kind node + Bidirectional mount
#   propagation), so the loop-mounted btrfs cache lives in the node's mount
#   namespace and node-stack-start.sh's `findmnt ... /space/cache` check reuses
#   it on every later container/pod start instead of mounting a second
#   instance of the same image.
# revert: restore the chart's emptyDir (main's rendering). Both roll the pods.
set -euo pipefail
export PATH=$HOME/.local/bin:$PATH
K=(kubectl --context kind-space-local -n space)
idx_vol=$("${K[@]}" get ds space-node-stack -o json | python3 -c 'import json,sys; v=json.load(sys.stdin)["spec"]["template"]["spec"]["volumes"]; print([i for i,x in enumerate(v) if x["name"]=="cache"][0])')
idx_mnt=$("${K[@]}" get ds space-node-stack -o json | python3 -c 'import json,sys; m=json.load(sys.stdin)["spec"]["template"]["spec"]["containers"][0]["volumeMounts"]; print([i for i,x in enumerate(m) if x["name"]=="cache"][0])')
case ${1:?apply|revert} in
  apply)  vol='{"name":"cache","hostPath":{"path":"/space/cache","type":"DirectoryOrCreate"}}'
          mnt='{"name":"cache","mountPath":"/space/cache","mountPropagation":"Bidirectional"}' ;;
  revert) vol='{"name":"cache","emptyDir":{}}'
          mnt='{"name":"cache","mountPath":"/space/cache"}' ;;
esac
"${K[@]}" patch ds space-node-stack --type=json -p "[
  {\"op\":\"replace\",\"path\":\"/spec/template/spec/volumes/$idx_vol\",\"value\":$vol},
  {\"op\":\"replace\",\"path\":\"/spec/template/spec/containers/0/volumeMounts/$idx_mnt\",\"value\":$mnt}]"
"${K[@]}" rollout status ds/space-node-stack --timeout=600s
