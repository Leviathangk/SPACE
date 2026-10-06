#!/bin/bash
# where.sh — print "<short id> <node> <state> <hp>" for every sandbox sbxmgr knows (both workers)
export PATH=$HOME/.local/bin:$PATH
for p in $(kubectl --context kind-space-local -n space get pods -l app.kubernetes.io/name=space-worker -o name | sed 's#pod/##'); do
  n=$(kubectl --context kind-space-local -n space get pod $p -o jsonpath='{.spec.nodeName}')
  kubectl --context kind-space-local -n space exec $p -c node-stack -- sh -c 'COLUMNS=400 space-sbxmgr list' 2>/dev/null | grep -E '^│' | grep -v 'ID (short)' | awk -F'┆' -v n=$n '{gsub(/[│ ]/,"",$1); gsub(/ /,"",$3); gsub(/ /,"",$4); print $1, n, $3, $4}'
done
