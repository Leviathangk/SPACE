# Natural guest death and the node-stack restart attestation effect (Kind reproduction)

Status: **DRAFT — experiment 2 complete, experiment 1 running.** The final
version replaces this line.

## Summary

- **Experiment 2: node-stack restart attestation effect. Root-caused; a
  Kind harness defect.**
  - A restart that starts a *new node-stack container* (container restart or
    pod replacement) strands every guest already running on that node. The
    guest stays alive, but the host can no longer reach it.
  - Restarting only sandbox-manager, only node-manager, or only a spaced-host
    process does not strand guests.
  - Cause: the Kind *worker* chart mounts `/space/cache` as an `emptyDir`
    without mount propagation. The entrypoint therefore loop-mounts
    `/var/lib/space-cache.img` again inside every new container's private mount
    namespace. With the old mount still held by the running jails, the kernel
    brings up a **second, independent btrfs instance of the same image file**
    (same fsid, new `st_dev`).
  - From then on, every host-side path lookup of a running jail's
    `…/root/v.sock` (and `firecracker.sock`) lands on a different superblock
    than the one Firecracker listens on. Node-manager's endpoint attestation
    sees the device change, and the spaced bridge dial gets a fast
    `transport error`.
  - Giving worker pods the builder role's hostPath + `Bidirectional` cache mount
    removes the effect (0 of 6 stranded vs 9 of 9).
  - EKS mounts `/space/cache` once per boot in the host namespace, so this
    mechanism should not apply there, pending a check of the production worker
    values, which are not in this repository.
- **Experiment 1:** see below.

## Public-safety note

The production pattern this work was asked to match comes from a private
deployment. It is described here only qualitatively. No deployment names,
dates, counts, tickets or links are copied. Everything below uses synthetic
sandboxes on a local Kind cluster.

## Environment

| | |
|---|---|
| Host | Linux 6.12 x86_64, KVM, 32 vCPU, 123 GiB, Docker 27.5.1 |
| Cluster | the Kind cluster from the first report (`kind-space-local`), Kind v0.30.0, K8s 1.34, two sandbox workers |
| Source | `main` @ **`84a1d9ea`**. On the branch, commit `1e143284` makes the tree outside `scripts/repro-dead-guest/` and the two reports byte-identical to `84a1d9ea`; the `REPRO ONLY` budget commit `d778c139` is not in effect. |
| node-stack image | `sha256:95fa398c…`, built from that tree with `infra/space/kind/scripts/build-images.sh space-node-stack` and rolled out at 14:19Z. Also used by `space-internal-api`, which was restarted onto it. |
| Binaries (both worker pods) | `space-sbxmgr-svc` sha256 `690945c7…` (byte-identical to the first report's NEW build: none of the four newer `main` commits touch sandbox-manager); `space-nodemgr-svc` sha256 `fbeffec2…` |
| Not rebuilt | `space-api`, `space-data-plane` and `space-cluster-controller` stay on the first report's `45c545cc` images. Between `45c545cc` and `84a1d9ea` their only code change is ECW Process.Start idempotency; this work uses the native API. |
| Object store | The cluster predates `main`'s SeaweedFS → versitygw switch (`8d02a71e`). `main`'s `bootstrap.sh` refuses such a cluster and asks for a teardown; the instruction was to reuse the running cluster, so the charts were **not** re-applied. The cluster keeps SeaweedFS, and only the images above changed. |
| Template | `seed:latest` (2 vCPU; 1 GiB and a 450 MB rootfs by default) |

The Kind node containers restarted once at about 14:12Z, before this work
started; the cause was outside this task. Every running VM died with them. The
three probes left by the first report (`repro-rst-*`) were captured, then
deleted (see "Earlier probes").

## Experiment 2 — the restart attestation effect

### Method

- Cohorts of healthy sandboxes on both workers:
  - "fresh": created, never suspended.
  - "restored": suspended, then woken with `:wake`, which restored them on the same node.
- After each trigger, every minute (`monitor.sh`, `probe-loop.sh`):
  - `POST /commands` (`sh -c 'echo ok'`), recording success and latency;
  - raw `CONNECT 5002` through `/proc/<firecracker pid>/root/v.sock` from the
    Kind node (`node_probe.py`);
  - sandbox-manager's composed health (`space-sbxmgr list`);
  - a copy of every worker pod's `/space/logs`.
- `mountcheck.sh` compares the `v.sock` `dev:ino` as the node-stack pod resolves
  it with the same file seen through Firecracker's own root.
- Single-service restarts use `restart-one.sh`. It points the entrypoint's pid
  file at a placeholder, sends the service SIGTERM, and re-executes it with its
  original argv, environment, uid/gid/groups and log. This restarts only that
  service: the entrypoint otherwise exits the whole container when any
  supervised pid dies (`scripts/k8s/node-stack-start.sh:484-507`).

### Trigger matrix

The restarted node is `worker`, with `worker2` untouched as the control, unless
noted. "Stranded" means Process.Start fails, composed health is `Degraded`, and
raw `CONNECT 5002` still answers `OK`.

| Trigger | What restarts | Stranded |
|---|---|---|
| T4 | `space-spaced-host-svc` for 2 sandboxes; the watchdog respawns it within 5 s | **0/2** |
| T2 | sandbox-manager only (in place) | **0/4**. Health re-initialized and was Healthy again within about 80 s. |
| T3 | node-manager only (in place) | **0/4**. One transient 503 `stream_unavailable` and one 15 s start during the restart, then Healthy. |
| T1b | the node-stack **container**, in the same pod (one service killed, so the entrypoint exits and kubelet restarts the container) | **4/4** (2 fresh, 2 restored) |
| T1 | the node-stack **pod** (`kubectl delete pod`; same path as `rollout restart`) on `worker2` | **5/5**, including 2 created *after* T1b on `worker` |
| P0 | patch rollout (both pods) | every guest still running on the old mounts (expected) |
| P-T1b / P-T1 | T1b on `worker` + T1 on `worker2`, **with the worker DaemonSet's `/space/cache` as hostPath + `Bidirectional`** (`cache-mount-patch.sh apply`) | **0/6** (2 fresh + 1 restored per node; unavailable only for about 60 s while node services restarted) |

In the first report the same symptom followed all three `rollout restart`s:
an image change NEW→OLD, OLD→NEW, and a same-image restart.

### What a stranded guest looks like

- Process.Start (native `POST /commands`) returns 503 `stream_unavailable`. The
  sandbox event is `guest_process_start_failed` with `reason=connect_spaced_host`
  and `error="spaced endpoint attestation failed"` (PermissionDenied,
  `spaced_bridge.rs:430`).
- `space-sbxmgr health`:
  - `SPACED_GUEST` fails;
  - `FIRECRACKER` reports `node-manager Firecracker state read failed (status
    Unavailable)`;
  - `SPACED_HOST` answers `pong`.

  Composed health is `Degraded`.
- The raw `CONNECT 5002` answers `OK` in 0.25–0.4 ms, on every one-minute sample
  of every stranded VM.
- The pre-suspend flush fails with `guest snapshot preparation before suspend
  failed: … failed to connect to spaced bridge guest at firecracker vsock
  /space/cache/firecracker/jails/firecracker/<id>/root/v.sock:5002: transport
  error`. That is the production death signature, word for word.
  - Shown by the earlier probes: after their 1 h idle stage, each logged 10
    such auto-suspend failures and then `auto-suspend retry budget exhausted`.
- `network_policy_update_failed` (`network policy generation conflict`,
  generation 1, `inherit_template_default`) repeats about every 60 s. It is a
  separate issue: it also appears on healthy sandboxes, patched cohort
  included. See "Side findings".

### Recovery

| Attempt (stranded guest) | Result |
|---|---|
| None, observed | **No self-recovery.** e2a-1 was untouched from 14:29Z to its deletion (≥ 60 min). The first report's probes, on the same sandbox-manager binary, stayed stranded from the 11:19Z restart until their logs end at 13:32Z, at least 2 h 13 min. |
| In-place sandbox-manager restart (fresh attestation from a new process) | no |
| spaced-host respawn (the watchdog relaunches it and the proof is re-captured) | no |
| Explicit `:suspend` | **Destroys the sandbox.** The planned teardown's first step pauses the VM through `firecracker.sock`, which is also under the jail root, so the connection is refused. The result is `fc_pause_failed` → `sandbox_quarantined` → API `failed`, and every later wake is refused (`sandbox is quarantined`). |
| Auto-suspend | Fails at the flush with `guest_fs_flush_failed`, then parks after 10 attempts (the first report's probes) |
| `:stop` then `:start` | **Recovers.** The stop finishes and the start cold-boots a new jail on the current mount; Healthy again. This loses guest memory state and anything not yet flushed to disk. |

### Affected cohorts

- **Running on the node at the restart:** all of them, fresh and restored
  alike.
- **Created or restored on that node after the restart:** Healthy. Their
  jails live on the new mount (`mountcheck`: pod view and Firecracker view
  agree).
- They stay Healthy only until their node's next container or pod restart, which
  strands them in turn (e2b-3/4 at T1; e2b-1/2 and the restarted e2a-6 at P0).

### Mechanism

**1. No secret or attestation key is lost.** Every hop to the guest finds it by
*path*:
- sandbox-manager → spaced-host, a Unix socket under
  `/space/run/firecracker/sandboxes/<id>/`;
- spaced-host → guest, `/space/cache/firecracker/jails/firecracker/<id>/root/v.sock`
  (`crates/libs/spaced/host/server/src/bridge.rs:892-907`, connect at
  `crates/libs/spaced/common/src/vsock.rs:71-105`, error text at `vsock.rs:137`);
- the watchdog's guest probe dials the same path itself.

The endpoint proof is captured by node-manager at activation
(`crates/libs/node/mgr/server/src/service/endpoint_attestation.rs:229-280`). It
holds pids, start times, executable dev/inode, the `v.sock` dev/inode/uid/gid,
and Firecracker's listening socket from `/proc/<pid>/net/unix`. It is stored in
sandbox-manager's SQLite under the hostPath `/space/run`
(`crates/libs/sbx/mgr/server/src/db.rs:10337-10373`), so it survives restarts.

**2. The check that fails.** `validate_sandbox_endpoint_proof`
(`endpoint_attestation.rs:125`) re-stats the socket path in node-manager's mount
namespace and compares it with the proof:

```rust
let socket_metadata = socket_metadata(&runtime.vsock_path)?;   // endpoint_attestation.rs:327
...  "vsock endpoint identity changed after activation"         // :334
```

The result is `FailedPrecondition`, which sandbox-manager reports as
PermissionDenied `spaced endpoint attestation failed` (`spaced_bridge.rs:412/430`).
The watchdog probe and the flush fail even earlier: their `UnixStream::connect`
to the path reaches a socket inode with no listener. That is the ~35 µs
`transport error`.

**3. Why the path resolves elsewhere after a restart.**
- For the worker role the Kind chart gives `/space/cache` an `emptyDir` with no
  mount propagation (`infra/space/kind/charts/space-node-stack/templates/daemonset.yaml:615-623`;
  only builders get `mountPropagation: Bidirectional` at `:525-527`).
- `ensure_btrfs_cache` (`scripts/k8s/node-stack-start.sh:107-196`) reuses an
  existing btrfs mount only if one is visible (`:110-113`). A new container
  never sees one, so it attaches a new loop device to the same
  `/var/lib/space-cache.img` and mounts it.
- The jailer's mount namespaces were cloned from the old container and keep the
  old mount alive.
- So do the per-sandbox `space-spaced-host-svc` and `space-netgtw-svc`
  processes, which runtime-supervisor launches in per-sandbox cgroups. They
  outlive the node-stack container: after a pod deletion they were found
  reparented to init, still holding the old pod's mount namespace, and the
  deleted image's loop device stayed attached until they were killed.

Observed with `mountcheck.sh` and `dmesg` at T1b:

```
/dev/loop3: [66311]:1099310 (/var/lib/space-cache.img)   # mounted before the restart (VMs use it)
/dev/loop4: [66311]:1099310 (/var/lib/space-cache.img)   # attached by the new container
BTRFS info (device loop4): first mount of filesystem 3eb62a85-…   # same fsid as loop3
pod-view 01a11197-…-e4c884da64e6 3145938:501    # new container resolves v.sock here
fc-root  01a11197-…-e4c884da64e6 2097241:501    # Firecracker listens here (same inode no., other st_dev)
```

T1 on `worker2` showed the same: `loop5` on `loop0`'s image, `3145956:*` vs
`2097240:*`. With the patch, both restarts logged `/space/cache already btrfs`,
and both views stayed identical (`3145961:*`, `3145973:*`).

**4. Proof.** `cache-mount-patch.sh apply` changes only that volume and its
mount to the builder's hostPath + `Bidirectional`. The btrfs mount then lives in
the Kind node's namespace and is reused, which made the effect disappear (0/6).
The suspend/wake round trip after the patched restarts also worked. The patch
is a local experiment. It was reverted before experiment 1, which runs on
`main`'s chart.

**5. Kind vs EKS.**
- On EKS, `/space/cache` is the instance-store NVMe, formatted and mounted once
  per boot in the host namespace (`infra/space/packer/scripts/base/11-nvme-cache.sh:139-144`,
  `infra/space/aws/eks/modules/space/templates/space-cache-nvme-mount.sh:81`).
- A pod restart there cannot produce a second instance or a second mount, and
  the code above depends only on path identity. **The mechanism should not
  apply to EKS.**
- Caveat: the production worker DaemonSet values
  (`infra/space/aws/eks/deploy/<env>/space-worker.values.yaml`, referenced
  from `node_groups.tf:360`) are not in this repository. Someone with access
  should confirm that worker pods mount the host's `/space/cache` as a hostPath
  (any propagation mode is enough, since the host mount pre-exists), not an
  emptyDir.
- The Kind README states the opposite of what was observed: it says the cache
  "outlives pod restarts like the NVMe does" (`infra/space/kind/README.md:740`).
  `persistImage` keeps the *data*, not the mount that live sockets are bound to.

**6. Kind side risk.**
- While stranded guests exist, two independent btrfs instances are writing to
  one image file. Each has its own allocator and its own transaction commits.
  This can corrupt the cache filesystem.
- Both workers' cache devices already showed non-zero `corrupt` counters
  (15 and 26) at their 14:12Z mount, from before this task.
- After this experiment's restarts, which left up to three instances of each
  image mounted at once, `btrfs scrub` found **3,944 and 2,964 uncorrectable
  checksum errors** on the two worker caches. The VMs that held the old
  instances had just exited, and the caches were back on a single instance.
  The errors were in jail files (for example a quarantined jail's `firecracker`
  binary copy).
- When the old instances unmounted, the kernel logged a different fsid for
  the duplicate (`e60a4e99…` vs the image's `3eb62a85…`). That is consistent
  with btrfs's temp-fsid handling of a second mount of an already-mounted
  filesystem.
- The data was not inspected file by file, so the double mount is the likely
  cause, not a proven one.
- Both images were recreated (the chart's documented clean start: delete the
  file on the node) and scrub-clean before experiment 1.
- This alone makes the harness fix worth doing: on today's chart, any Kind test
  that restarts the node-stack with live VMs can corrupt the worker cache.

### What it implies for production

- If the production chart mounts the host cache, node-stack rollouts do not
  strand guests this way. The production dead-guest population then needs a
  different cause. Experiment 1 looks for one.
- What carries over is the **diagnostic**:
  - A raw `CONNECT 5002` through `/proc/<fc pid>/root/v.sock` that answers `OK`
    while the bridge reports `transport error` proves the guest is alive and the
    host-side path is broken.
  - A raw connect that hangs or fails proves the guest is hung or dead.
  - Production triage of dead guests should run this check before concluding
    that guests are dead.
- **Product gaps the Kind case exposed, which would matter for any host-side
  path breakage:**
  - Nothing re-attests or rebinds a running guest's endpoint after a restart.
    Startup recovery only re-adds monitoring (`watchdog.rs:1007-1043`).
  - Composed `Degraded` drives no remediation.
  - An explicit suspend of such a sandbox quarantines it, and the API then
    reports it `failed`.

### Recommended next steps (experiment 2)

1. **Kind harness fix:** give worker pods the builder's `/space/cache`
   treatment (hostPath + `Bidirectional`), as `cache-mount-patch.sh` does.
   Correct the README claim at `:740`. This removes a class of
   false "dead guests" from every Kind test that restarts the node-stack, and
   the double-mounted btrfs risk with it.
2. **Production check:** confirm that the worker values mount the host
   `/space/cache`, so this harness gap is not shared.
3. **Product hardening (optional):** when a running sandbox's
   endpoint proof fails with `vsock endpoint identity changed` while
   `/proc/<pid>/root/v.sock` answers, sandbox-manager could log it as a distinct
   reason. That would separate "host path broken" from "guest hung" in metrics.

## Experiment 1 — natural guest death after restore

### Workload (`asi_workload.py`, one process per sandbox; `run-exp1.sh`)

The repository has no ASI template or recipe. The closest recipe,
`templates/firecracker/recipes/code-runtime.toml`, needs a networked builder
pool. The workload therefore emulates an ASI worker on `seed:latest` (python3
and curl, no git).

**Sandbox:** 2 vCPU, `memory_mib` 2048 (the guest sees `MemTotal` 1991 MiB;
Firecracker reports `memory_mib` 2176), `disk_mib` 4096. Auto-lifecycle is
access-idle → suspend at 8 h, so it never fires during the run.

**At every connect** (after create and after every restore):
- 9 CLI wrapper scripts, the memory grower, and a launcher are backfilled with
  `files:write`;
- three long-lived processes are ensured: `python3 -m http.server`, a log
  writer, and `memhog.py`.
- `memhog.py` keeps `/tmp/memtarget` MB resident in 16 MB chunks with every
  page touched, and re-touches part of the set every 10 s. The driver raises
  the target every 5 min from 200 MB toward 1500 MB over 4 h (about 75% of
  the limit).
- The long-lived processes survive suspend/restore (same PIDs after restore).

**Busy loop:**
- a short command every 5–20 s, picked at random from:
  - shell `ls`/`df`;
  - python (sha256 of 1 MiB; JSON of 20k objects);
  - a VCS-like tree hash (`find | sha1sum`, standing in for git);
  - one of the wrappers;
  - `curl` to the local server;
  - 2 MB workspace writes with rotation;
  - `ps`.
- telemetry every 60 s (`free`, loadavg, memhog MB, live process count, and
  `dmesg | tail` every 5th time);
- a 1–6 MB `files:write` upload plus a verified `files:read` download every
  2–3 min.

All calls go through the native API (`POST /commands` is the native
Process.Start), and every call is recorded with its latency.

**Cycles:**
- create, then 10 min warm-up;
- then 3 cycles, each: explicit `:suspend` → wait until the snapshot is
  portable (the smoke suite's `RESTORE_PORTABLE_SQL`) → `:wake` with the
  *other* sandbox worker cordoned → 60 min busy.
- Odd labels restore cross, same, cross; even labels same, cross, same.
- Restores are serialized cluster-wide.
- Cross-node restores pull `rootfs.btrfs-send` and the memory image from
  object storage (`runtime_activated`: `boot_type=snapshot`,
  `restore_provenance=remote_matches_local`, `remote_snapshot_staged=true`).
  Same-node restores are `local_only`.

**Detection:**
- Three consecutive failed operations open an incident. `capture.sh` then
  records:
  - raw `CONNECT 5002`;
  - Firecracker thread states and wchan;
  - `fc.log` and the Firecracker API state;
  - `space-sbxmgr health/doctor`;
  - `space-nodemgr inspect`;
  - 30 min of node-stack log lines for the sandbox;
  - a guest dmesg attempt;
  - a 30 min follow-up of vsock and health.
- The workload keeps probing every 15 s.

**Per minute, host side (`monitor.sh`):** raw `CONNECT 5002` and latency,
Firecracker RSS and CPU for every VM, `space-sbxmgr list` (composed health),
disk headroom, and a copy of `/space/logs`. The DogStatsD sink records
`space.sandbox.guest.health`.

### Attempt 1 (aborted, harness artifact)

- Attempt 1 started at 15:33Z.
- At 15:46:58Z the Kind nodes' shared `/var` filesystem filled up. That
  filesystem is the devbox's 50 GB Docker data disk, about 35 GB of which is
  shared Docker images and build cache, which this task does not prune.
- The sparse cache images could no longer allocate
  (`critical space allocation error, dev loopN`; btrfs `Transaction aborted
  (error -5)`, `forced readonly`).
- Within 30 s, 8 of 12 sandboxes failed `files:write` with `Read-only file
  system (os error 30)` and commands with 500. Postgres, on the same disk,
  stopped accepting writes.
- This was not a guest death. The worker cache images and the SeaweedFS data
  were moved to the 400 GB home disk with bind mounts inside the Kind node
  containers (same code paths), and the run was restarted with a disk guard.
- Evidence is kept outside the repository.

### Attempt 2 — baseline (15:57:08Z → 19:12:52Z)

All 12 sandboxes ran their full plan.

| | |
|---|---|
| Busy after restore | **35.9 sandbox-hours** (36 × 60 min), plus 2.0 sandbox-hours of post-create warm-up |
| Restores | **36: 18 cross-node** (snapshot pulled from object storage), **18 same-node** (`local_only`); all succeeded in 2–10 s; landed on the intended node every time |
| Operations | **16,870, 0 failed**: 6,077 Process.Start-equivalent commands of the short mix, 2,280 telemetry, 913 upload + 913 verified download, 528 wrapper backfills, 48 connects |
| Latency p50 / p99 / max (ms) | short commands about 40–75 / 60–220 / ≤ 1,550; upload 65 / 153 / 482; download 21 / 41 / 78 |
| Memory | limit 2048 MiB (guest `MemTotal` 1991 MiB); memhog reached 1,200 MB (ramp 200 → 1500 MB over 4 h, so the 3 h 15 min baseline reached 1,200); peak Firecracker RSS 1,341–1,368 MB |
| Composed health (`space-sbxmgr list`, per minute, about 190 samples per sandbox) | never `Degraded`. Outside each sandbox's own suspend/restore windows: only `✅`, one `⏳` at creation, and a few `suspended` samples while the snapshot became portable |
| `space.sandbox.guest.health{lifecycle:started,health:degraded}` | 0 in every one of 196 minutes |
| Raw `CONNECT 5002` (2,280 samples) | 0 failures outside suspend/restore windows; 9 three-second timeouts, each during that VM's own suspend (VM paused for the snapshot) |
| sbxmgr log, these sandboxes | only `Healthy` composed-health transitions (48); no `guest_process_start_failed`, no `suspend_failed` |

**Coverage gap:**
- Every suspend in attempt 2 used the **Full** rootfs codec, not btrfs-send:
  `btrfs-send precondition not met; suspending with Full codec instead`, 36 times.
- When the workers came back on fresh cache images, template-manager's on-demand
  pull of `seed` ran before node-manager's socket was up (`nodemgr btrfs
  subvolume create failed; falling back to plain directory … failed to connect
  to node-manager … transport error`). The durable retry then failed for good
  (`digest dir is not a btrfs subvolume; btrfs-send codec permanently
  unavailable for this build`).
- So the baseline's cross-node restores staged a Full rootfs and the memory
  image from object storage. They did not use the btrfs-send codec named in the
  production pattern. The pre-reset smoke run did use btrfs-send
  (`rootfs.btrfs-send`). See "Side findings" for the race.

## Side findings

- `network_policy_update_failed` (`network policy generation conflict`) is
  emitted about every 60 s per sandbox, on healthy sandboxes as well. node-ctl
  re-pushes generation 1 with a body that differs from the stored row
  (`grpc_lifecycle.rs:2269-2279`, `db.rs:10579-10584`, re-push loop
  `node/ctl/server/src/lib.rs:71`). Not investigated further, and not linked to
  guest reachability.

## Earlier probes (from the first report)

`repro-rst-fresh`, `repro-rst-restored` and `repro-rst2-fresh` stayed `Degraded`
from their restarts (11:04/11:19Z) until the logs end (13:32Z), and logs are
missing from 13:32 to 14:12Z. At 14:12Z the Kind containers restarted, their VMs
died, and the API marked them `failed`. Each reached its 1 h idle stage, failed
10 auto-suspends with the `transport error` flush failure, and parked:

| Probe | Degraded from | Suspend attempts | Parked |
|---|---|---|---|
| rst-fresh | 11:20:48Z | 12:07–12:47Z | 12:47:53Z |
| rst-restored | 11:20:38Z | 12:01–12:42Z | 12:42:28Z |
| rst2-fresh | 11:20:58Z | 12:22–13:02Z | 13:02:38Z |

Their state was captured, then they were deleted (`DELETE /v1/sandboxes/{id}`).
