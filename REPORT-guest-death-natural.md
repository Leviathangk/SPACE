# Natural guest death and the node-stack restart attestation effect (Kind reproduction)

## Summary

**Experiment 1: no natural guest death was reproduced.**
- Coverage:
  - 12 sandboxes ran an agent-like busy workload through **72 restores**: 38
    cross-node (26 with the Full rootfs codec, 12 with btrfs-send) and 34
    same-node.
  - **61.9 sandbox-hours** of busy time after restore, plus 5 sandbox-hours
    after create.
  - **30,184 Process.Start and file operations: 0 failed.**
- Composed health was never `Degraded`. The
  `space.sandbox.guest.health{lifecycle:started,health:degraded}` gauge read 0
  in every minute, and raw `CONNECT 5002` never failed outside a VM's own
  suspend or boot.
- The two stress variants also passed:
  - memory at about 97% of the guest's limit, with all 12 restored concurrently
    onto one node;
  - restores right after a node-stack restart.
- With 0 deaths in 72 restores, the per-restore chance of a permanent death in
  this setup is below about 4% (95% one-sided, rule of three). For deaths within the
  first hour after a restore, 48 restores were watched for the full 60 min,
  giving a bound below about 6%.
- That does not rule out a production rate that is rare per restore. It does
  show the deaths are not a generic consequence of suspend/restore, cross-node
  restore, memory pressure or restore load as reproduced here.

**Experiment 2: the restart attestation effect is root-caused. It is a Kind
harness defect that should not apply to EKS.**
- A restart that starts a *new node-stack container* (container restart or pod
  replacement) strands every guest already running on that node. The guest is
  alive, but the host can't reach it.
- Restarting only sandbox-manager, only node-manager, or only spaced-host
  doesn't.
- Cause:
  - The Kind *worker* chart mounts `/space/cache` as an `emptyDir` with no mount
    propagation, so every new container loop-mounts the same cache image again.
  - The running jails (and leftover per-sandbox sidecars) keep the old mount, so
    the kernel runs **two independent btrfs instances of one image file**.
  - Host-side path lookups of a running jail's `v.sock`/`firecracker.sock` then
    land on the new superblock, where nothing listens.
- The resulting failures match the production signature word for word:
  - node-manager's endpoint attestation fails (`vsock endpoint identity
    changed`, surfaced as `spaced endpoint attestation failed`);
  - the bridge dial and the pre-suspend flush fail with `failed to connect to
    spaced bridge guest at firecracker vsock …/v.sock:5002: transport error`;
  - composed health goes `Degraded`, the auto-suspend budget parks the sandbox,
    and the gauge steps up at the restart.
- Nothing recovers it except stop/start, and an explicit suspend quarantines the
  sandbox. The worker cache filesystems also ended up corrupted, with thousands
  of uncorrectable checksum errors.
- Using the builder role's hostPath + `Bidirectional` cache mount for workers
  removes the effect: **0 of 6 stranded, against 9 of 9 without it.**
- EKS mounts `/space/cache` once per boot in the host namespace, so the
  mechanism should not apply there. The production worker values aren't in this
  repository, though, so someone with access needs to confirm.

**Side findings:**
- a template-manager / node-manager startup race that silently disables the
  btrfs-send codec for a template build on a fresh node;
- per-sandbox sidecars outliving the node-stack pod;
- node-to-API completion lag under concurrent restores;
- the Kind harness's shared Docker disk filling up, which looks like a mass
  guest death.

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

## Experiment 1 — natural guest death after restore

### Workload (`agent_workload.py`, one process per sandbox; `run-exp1.sh`)

The repository has no recipe for the production agent template. The closest recipe,
`templates/firecracker/recipes/code-runtime.toml`, needs a networked builder
pool. The workload therefore emulates an agent worker on `seed:latest` (python3
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
| Operations | **16,870, 0 failed**: 14,516 commands (native Process.Start), of which 10,819 are the short mix, 2,280 telemetry, 913 upload rotations, 408 memory-target updates and 96 connect/mkdir; plus 2,354 file calls (913 uploads, 913 verified downloads, 528 backfill writes) |
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

### Stress A — memory near the limit + 12 concurrent restores onto one node (19:13Z → 20:22Z)

Same 12 sandboxes after the baseline (`stress.py burst --mem-mb 1750
--target-node space-local-worker`, then `agent_workload.py --attach`).

- memhog was raised to 1,750 MB and every guest reached it (1,744 MB resident).
  Guest `free` showed 1,926–1,947 of 1,991 MiB used and 44–86 MB available for
  the rest of the run.
- All 12 were suspended concurrently, each taking 51–161 s with near-full
  memory images. Portable 4 s after the last suspend.
- With `space-local-worker2` cordoned, all 12 were woken at once and landed on
  `space-local-worker`: **8 cross-node, 4 same-node, all succeeded**.
- API-side wake durations were 14 s to 303 s. For a02, the slowest, the node
  log shows three concurrent artifact pulls finishing at 19:19:07,
  `restore_completed` at 19:19:10, and the API operation completing at
  19:21:57. The node-to-API lag is the same symptom as the first report's
  "API shows a stale state" oddity, not a guest problem.
- After that, 60 min busy at that memory level: **5,324 operations, 0
  failed, no incident, no guest OOM kill** (telemetry dmesg),
  `started/degraded` gauge 0 in every minute, and no raw vsock failure.

### Stress B — restores right after a node-stack restart, btrfs-send codec (20:23Z → 21:30Z)

- **Fresh cohort.** The 12 baseline sandboxes were deleted, and the
  directory-form `seed` build was removed (`space-tmplmgr delete-build`) and
  re-pulled with node-manager up. Suspends then logged `btrfs-send codec: rootfs
  encoded as btrfs send -p stream`.
- Then 12 new sandboxes of the same shape: 15 min busy warm-up with memhog
  ramped to 1,200 MB (0 failures).
- **Burst** (`stress.py burst --restart-node-stack`):
  - all 12 were suspended concurrently and were portable after 4 s;
  - `kubectl rollout restart ds/space-node-stack` replaced both worker pods while
    every sandbox was suspended. One loop device per image remained, so the
    restart was clean.
  - All 12 were woken concurrently 60 s later, with no cordon. **All 12
    succeeded in 17–41 s**, and the planner put every one back on its source
    node (12 same-node).
- 45 min busy afterwards: **4,025 operations, 0 failed, no incident**.

### Final cycle — cross-node restores through the btrfs-send codec (21:31Z → 21:58Z)

- Same 12 sandboxes, one more cycle: explicit suspend → portable → cordoned
  cross-node wake.
- **All 12 landed on the other node** and staged `rootfs_btrfs_send` from
  object storage (`restore_provenance=remote_matches_local`).
- Then 25 min busy: 0 failed operations, no incident.
- Across the second cohort (warm-up, stress B and this cycle): 7,954 operations, 0 failed. The gauge was 0
  in all 96 minutes. The only raw-vsock failure came seconds after a VM booted.

### Coverage (experiment 1, attempt 2 + stress variants)

| | Restores | Post-restore busy | Operations (failed) |
|---|---|---|---|
| Baseline, 3 cycles | 18 cross (Full codec) + 18 same | 35.9 sandbox-h (60 min each) | 16,870 (0) |
| Stress A: memory 1,750/1,991 MiB, 12 concurrent onto one node | 8 cross (Full) + 4 same | 12.0 sandbox-h (60 min each) | 5,360, including the orchestrator's 36 (0) |
| Stress B: restore right after node-stack pod replacement | 12 same (btrfs-send snapshots) | 9.0 sandbox-h (45 min each) | 4,025, plus 1,442 in the new cohort's warm-up (0) |
| Final cycle: cross-node via btrfs-send | 12 cross (btrfs-send) | 5.0 sandbox-h (25 min each) | 2,307, plus 180 at reconnect (0) |
| **Total** | **72: 38 cross-node, 34 same-node** | **61.9 sandbox-hours** (+ 5.0 post-create) | **30,184 (0)** |

**Deaths found: none.**
- No sandbox opened an incident.
- No composed-health transition other than `Initializing → Healthy` was logged
  for any experiment-1 sandbox.
- No `guest_process_start_failed` or `suspend_failed`.
- Raw `CONNECT 5002` never failed outside a VM's own suspend or boot.

Wall clock for attempt 2 was 15:57Z → 21:58Z, about 6 h. Attempt 1 (aborted)
used another 20 min.

### What it implies for production

- The production pattern is permanent deaths within the first hour after a
  restore, all on the production agent template. It does **not** come from suspend/restore itself, from
  cross-node restore through object storage (Full or btrfs-send codec), from
  memory near the sandbox limit, or from many concurrent restores on one node,
  at least on `seed` with this workload.
- What is still not covered, and is a candidate cause:
  - **The production agent template and guest kernel.** On `seed` the guest
    runs a small userland, and `kernel_version` here is `legacy-e20e46d0c36c`.
  - **Real host pressure.** NVMe cache, many more VMs per node, CPU
    overcommit, and real S3 latency or throughput during restores.
  - **The real agent workload's I/O and network behaviour.** Egress, large
    repositories and git, many concurrent processes.
  - **Restore generations.** Sandboxes restored dozens of times over days,
    not 3–4 times in hours.
  - **Production's actual per-restore death rate.** It may be below what 72
    restores can detect.
- The production signature (`…v.sock:5002: transport error` → `Degraded` →
  `guest_fs_flush_failed`) is exactly what experiment 2 produced with a live
  guest and a broken host path. The message alone does not distinguish "guest
  dead" from "host can't reach a live guest".

### Recommended next steps (experiment 1)

1. **On the next production dead guest, run the raw check before anything
   else:** `CONNECT 5002\n` on `/proc/<firecracker pid>/root/v.sock` from the
   node (`node_probe.py` here).
   - `OK` means the guest is alive and the problem is host-side. Then compare
     `stat` of the jail's `v.sock` (dev:ino) from sandbox-manager's and
     node-manager's namespaces against Firecracker's view, as `mountcheck.sh`
     does.
   - No answer means the guest is hung. Then capture Firecracker's per-thread
     state and wchan (`capture.sh`) and `fc.log`.
2. **Get guest-side evidence from a canary pool.**
   - Enable `SPACE_ENABLE_GUEST_CONSOLE_LOGS=1` there, so serial console and
     dmesg survive a hung agent.
   - It only applies to cold-booted sandboxes and their snapshots, so it needs
     a fresh cohort.
3. **Correlate dead guests with restore attributes already in
   `runtime_activated`.** Look at `restore_provenance`, rootfs codec,
   `kernel_version`, template digest, `memory_mib` and node, and check how often
   the node logged the btrfs-send fallback from side finding 1.
4. **If a Kind follow-up is wanted:** run the same driver against the
   production agent template, built with the `code-runtime` recipe as a stand-in
   if needed, with real network egress and days-long restore chains. Keep the
   Kind cache images off the shared Docker disk.

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

## Side findings

1. **Template-manager / node-manager startup race (product-relevant).**
   - On a node-stack start with an empty cache, template-manager's on-demand
     pull of the latest template ran before node-manager's socket accepted
     connections. It then fell back to a plain directory (`on-demand pull:
     nodemgr btrfs subvolume create failed; falling back to plain directory …
     failed to connect to node-manager at
     /space/run/node-manager/node-manager.sock: transport error`).
   - The durable retry then gave up (`job failed permanently … digest dir is
     not a btrfs subvolume; btrfs-send codec permanently unavailable for this
     build`). Every later suspend of that build on that node silently used the
     Full rootfs codec (`btrfs-send precondition not met; suspending with Full
     codec instead`).
   - It cleared only after `space-tmplmgr delete-build`, once nothing
     referenced the build, followed by a re-pull.
   - Seen on both workers at once. On EKS a freshly booted node also starts with
     an empty cache, so it may apply there. Worth checking how often production
     suspends log that fallback.
   - Suggested fix: have the pull wait for node-manager (or retry the subvolume
     create) instead of committing a directory-form build, and make the stamp
     job retryable.
2. **Per-sandbox sidecars outlive the node-stack pod.**
   - `space-spaced-host-svc` and `space-netgtw-svc` run in per-sandbox cgroups
     and survive the node-stack container. After a pod deletion they were found
     reparented to init, pinning the old pod's mount namespace and its btrfs
     instance.
   - This is part of why old cache instances stay mounted on Kind, and why a
     deleted cache image's space was not released.
3. **API completion lags node-side restore by up to about 3 min under
   concurrent restores** (stress A, a02). This is the same symptom as the first
   report's "API state stale" oddity. Not investigated.
4. **Kind harness: shared Docker data disk.**
   - The Kind nodes' `/var` (cache images, SeaweedFS, Postgres) shares the
     devbox's 50 GB Docker data disk with about 35 GB of shared images and build
     cache. Twelve 2 GiB sandboxes with memory snapshots filled it in 14 min.
   - btrfs on the loop images then went read-only and guests got EIO, which
     looks like a mass guest death.
   - Long Kind runs should keep cache images off that disk: this run used bind
     mounts into the nodes' `/var/lib/containerd`, which is on the home disk.
5. `network_policy_update_failed` (`network policy generation conflict`,
   generation 1, `inherit_template_default`) is emitted about every 60 s per
   sandbox, healthy ones included.
   - node-ctl re-pushes generation 1 with a body that differs from the stored
     row (`grpc_lifecycle.rs:2269-2279`, `db.rs:10579-10584`, re-push loop
     `node/ctl/server/src/lib.rs:71`).
   - Not linked to guest reachability; not investigated further.

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

## Reproduce

Run from the repository root with the Kind stack up (`docs/installation.md`) and
the API port-forwarded on `127.0.0.1:8080` and `:8090`. All scripts are in
`scripts/repro-dead-guest/`.

**Experiment 2, trigger matrix:**
- Start the probes and monitor:
  `driver.py create <label> --idle 6h`, then
  `monitor.sh <out> 60` and `probe-loop.sh <ids> <out>/probe.jsonl`.
- Triggers:
  - `restart-one.sh sbxmgr|nodemgr`, run inside the node-stack container;
  - kill a `space-spaced-host-svc`;
  - kill any supervised service, which restarts the container;
  - `kubectl delete pod`.
- Inspect with `mountcheck.sh` and `e2-summary.py <out>`.
- Proof: `cache-mount-patch.sh apply`, repeat the triggers, then `revert`.
  Afterwards run `reset-cache-mounts.sh` and `recreate-cache-images.sh` to
  return to a single, clean cache instance.

**Experiment 1:**
- Run `run-exp1.sh <out> 12` and `monitor.sh <out>/mon 60`. Watch with
  `watch-exp1.sh`, summarize with `exp1-summary.py <out> <out>/mon`.
- Stress variants:
  `run-stress.sh <out> <name> <phase> <cycle> <busy-secs> <mem-mb> -- [--mem-mb N] [--target-node NODE] [--restart-node-stack]`.

Derived evidence is in `scripts/repro-dead-guest/evidence/phase2/`. Raw logs
stay outside the repository.

## State left on this box

- Kind cluster `space-local` is up, on `main` 84a1d9ea's node-stack image and
  chart (DaemonSet template identical to the pre-experiment rendering). It has
  no sandboxes.
- Harness deviations still in place, in the Kind node containers only:
  - the worker cache images are bind-mounted from
    `/var/lib/containerd/repro-space-cache/` (the host's
    `~/.kind-data/<node>/containerd`);
  - SeaweedFS data is bind-mounted from `/var/lib/containerd/repro-seaweedfs`.
  - Both are lost if a Kind node container restarts. To undo, scale the
    consumers down, unmount, and delete those directories.
- The statsd sink containers and the monitors are stopped. Port-forward loops
  (`pf.sh`, outside the repo) keep the API reachable; stop them with
  `pkill -f pf.sh`.
