# DIAGNOSTIC ONLY (diag/oci-busy-unmount-trace). Never merge.
p="$1"
out=/space/logs/oci-busy-diag.log
{
echo "=== OCI-BUSY-DIAG $(date -Ins) path=$p"
mid=$(awk -v p="$p" '$5==p{print $1}' /proc/self/mountinfo | tail -1)
echo "self_mnt_ns=$(readlink /proc/self/ns/mnt) mnt_id=$mid"
echo "--- self mountinfo under builds/oci"
grep -F "/builds/oci" /proc/self/mountinfo
echo "--- children of mnt_id $mid"
awk -v m="$mid" '$2==m' /proc/self/mountinfo
echo "--- copies per mount namespace"
declare -A seen
for d in /proc/[0-9]*; do
  ns=$(readlink "$d/ns/mnt" 2>/dev/null) || continue
  [[ -n "${seen[$ns]:-}" ]] && continue
  seen[$ns]=1
  echo "ns=$ns first_pid=${d#/proc/} comm=$(cat "$d/comm" 2>/dev/null)"
  grep -F "space-oci-rootfs" "$d/mountinfo" 2>/dev/null | sed 's/^/    /'
done
echo "--- holders (cwd/root/exe/fd/maps)"
for d in /proc/[0-9]*; do
  pid=${d#/proc/}; c=$(cat "$d/comm" 2>/dev/null)
  for k in cwd root exe; do
    t=$(readlink "$d/$k" 2>/dev/null)
    [[ "$t" == *space-oci-rootfs* ]] && echo "pid=$pid comm=$c $k=$t ns=$(readlink $d/ns/mnt)"
  done
  for f in "$d"/fd/*; do
    t=$(readlink "$f" 2>/dev/null) || continue
    m=$(awk '/^mnt_id:/{print $2}' "$d/fdinfo/${f##*/}" 2>/dev/null)
    if [[ "$t" == *space-oci-rootfs* || ( -n "$mid" && "$m" == "$mid" ) ]]; then
      echo "pid=$pid comm=$c fd=$f -> $t mnt_id=$m"
    fi
  done
  grep -F space-oci-rootfs "$d/maps" 2>/dev/null | sed "s/^/pid=$pid comm=$c maps: /"
done
echo "--- processes (newest 60)"
ps -eo pid,ppid,etimes,stat,comm,args --sort=start_time 2>/dev/null | tail -60
echo "--- retry umount every 100ms for up to 10s"
ok=0
for i in $(seq 1 100); do
  if umount "$p" 2>/tmp/oci-busy-umount.err; then echo "RETRY_OK attempt=$i elapsed_ms=$((i*100))"; ok=1; break; fi
  sleep 0.1
done
[[ $ok == 1 ]] || { echo "RETRY_FAILED: $(cat /tmp/oci-busy-umount.err)"; echo "--- holders after 10s"; for d in /proc/[0-9]*; do t=$(readlink "$d/cwd" 2>/dev/null); [[ "$t" == *space-oci-rootfs* ]] && echo "pid=${d#/proc/} cwd=$t"; done; grep -F space-oci-rootfs /proc/self/mountinfo; }
echo "=== END OCI-BUSY-DIAG"
} 2>&1 | tee -a "$out"
