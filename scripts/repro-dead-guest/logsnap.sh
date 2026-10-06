#!/bin/bash
# logsnap.sh OUT_DIR — copy every sandbox-worker node-stack pod's /space/logs
# (including previous-NNNNNN/ container attempts) to OUT_DIR/<pod>/. /space/logs
# is an emptyDir, so a replaced pod's logs survive only in the last copy.
out=${1:?out dir}
export PATH=$HOME/.local/bin:$PATH
for p in $(kubectl --context kind-space-local -n space get pods -l app.kubernetes.io/name=space-worker -o name 2>/dev/null | sed 's#pod/##'); do
  mkdir -p "$out/$p.tmp"
  if kubectl --context kind-space-local -n space exec "$p" -c node-stack -- tar -C /space/logs -cf - . 2>/dev/null | tar -C "$out/$p.tmp" -xf - 2>/dev/null; then
    rm -rf "$out/$p"; mv "$out/$p.tmp" "$out/$p"
  else
    rm -rf "$out/$p.tmp"
  fi
done
