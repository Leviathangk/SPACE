# Guest file and disk I/O

Status: implemented contract

How a sandbox's files reach its disk and come back out: the virtual disk and
its limits, the guest's upload and read paths, and when written data is
published and when it persists.

## The virtual disk

Each sandbox's root filesystem is an ext4 image attached to the Firecracker VM
as a virtio block device.

- **Cache mode.** The drive uses Firecracker's default `Unsafe` cache mode. The
  device offers no flush, so a guest `fsync` or `syncfs` completes once its
  writes reach the host's page cache. Nothing in the guest makes data survive
  a host failure; the host persists the image through its own writeback, and
  through snapshots for the lifecycle operations that take them.
- **One segment per request.** The device does not advertise more than one
  data segment per request (`VIRTIO_BLK_F_SEG_MAX`), so the guest sends one
  request per physically contiguous run of memory. Page-cache pages are
  scattered in a guest that has run a while, so buffered writeback and
  journal commits mostly reach the disk as 4 KiB requests. Large requests
  need physically contiguous memory, which only huge pages provide reliably.
- **Transparent huge pages.** The guest kernel boots with
  `transparent_hugepage=never`, and `spaced.service` switches it to `madvise`
  before the daemon starts, with synchronous and khugepaged defragmentation
  off. Only memory that asks for huge pages (`MADV_HUGEPAGE`), such as the
  upload buffers below, gets them, and a fault never stalls on compaction. A
  guest without these settings still serves uploads, in smaller requests.

## The drive limiter

Every sandbox's root drive has a Firecracker rate limiter with two token
buckets: operations (requests) and bandwidth (bytes). The shipped defaults are 10,000
operations/s and 100 MiB/s; a warm pool can assign other limits, and each
sandbox carries its own.

The operations budget counts guest requests even when the host serves them from
page cache. At 4 KiB per request, 1,000 operations/s restricts a busy guest to
about 3.9 MiB/s despite the separate 100 MiB/s bandwidth allowance. The higher
default reduces this small-file bottleneck; it is not a promise of 100 MiB/s for
every request size or of latency under arbitrary host contention.

Each bucket holds `disk_io_burst_secs` (sandbox-manager configuration, default
30) seconds of its sustained rate and refills at that rate. The sustained
rates set a sandbox's long-run share of the node's disk. The burst lets an
idle sandbox spend up to that much at once, as fast as the device takes it:
commands that touch thousands of small files, such as reading a cold Python
install, and uploads arrive in bursts.

Every idle sandbox holds its credit, so many sandboxes spending it together
load the host's page cache and disk at once. The per-sandbox limiter does not
make a dense node fair: at hundreds of sandboxes each one's fair share can be
below its limit. Use the aggregate host disk budget below to bound physical
I/O from all sandboxes together. This does not provide per-sandbox scheduling
fairness or bound requests served from the host's page cache.

### Upgrading the default allowance

Set `SPACE_DISK_IO_DEFAULT_MAX_OPERATIONS_PER_SEC` and
`SPACE_DISK_IO_DEFAULT_MAX_BANDWIDTH_MIB_PER_SEC` together on API (including the
Admin API), controller, and sandbox-manager processes to hold or change the
creation default independently of a binary upgrade. For example, `1000` and
`100` retain the previous shipped rates. Unset means the shipped `10000` / `100`.
Partial, malformed, zero, or overflowing pairs fail startup. The same resolver
handles unassigned cold creates, inherited policies, new manual pools, and legacy
wire/snapshot metadata with both limits absent. Explicit and saved assignments
are never clamped or rewritten by this setting. Configure all writers and workers
consistently; a partial rollout can assign different defaults.

Sandbox-manager also accepts `SPACE_DISK_IO_BURST_SECS` (positive u32), overriding
`disk_io_burst_secs` in its TOML file. Set it to the verified existing duration
while qualifying a rate change. Existing running VMs are not patched by changing
configuration, but subsequent cold starts and snapshot restores rebuild the
limiter from the saved rate and the receiving worker's burst duration. Thus a
saved 1,000-IOPS sandbox can acquire a 30,000-token bucket without changing its
saved rate. A 10,000-IOPS sandbox with 30-second bursts has 300,000 tokens. Test
many fully credited guests starting I/O together, including lifecycle latency,
not only a steady-state victim under background load.

The configured creation default applies to new unassigned sandboxes in direct,
Kubernetes, and node-local creation paths, and to warm-pool policies whose disk
I/O fields are absent. It requires updated API, pool-controller, and worker
binaries; rebuilding a guest image or template alone does not change the limit.
The shipped bandwidth and 30-second burst duration are unchanged by the rate change.
At the new rate the operations bucket holds 300,000 requests, so validate the
combined post-idle burst as well as sustained load. Older binaries may use a
different burst duration; inspect the actual root-drive limiter during rollout.

| Existing state | Upgrade behavior |
| --- | --- |
| Running or persisted sandbox with an assigned limit | Keeps its limit on start, restore, and ordinary lifecycle operations. |
| Rolling warm-pool policy with omitted I/O fields | Reconciliation rewrites the config to the configured default. All slots with the old assignment immediately become unclaimable; mismatched warming/ready slots are deleted before bounded refill. This can empty the pool. |
| Explicit rolling policy | Keeps its stored limit. Update it deliberately through the rolling-policy Admin API. |
| Existing manually managed pool configuration | Keeps its stored limit, including when editing pool size. The manual-pool API does not expose I/O overrides; newly created configurations use the current default. |
| Legacy snapshot or transfer metadata omitting both I/O fields | Reconstructing its sandbox row uses the receiving worker's configured default; present pairs are preserved, partial pairs are rejected. |
| Legacy writer omitting database columns | Retains the historical SQL fallback. Current writers bind effective limits explicitly; no schema migration or limit backfill is required. Historical migration comments describe the defaults at their creation, not the current deployment default. |
| Mixed old/new services or old materialized pool configuration | New creates can receive different defaults until all writers and pools converge. Inspect the saved assignment rather than assuming upgrade completion. |

Before upgrading writers, inventory inherited policies and pin each to its
verified current effective pair through the Admin API. Do not assume every
policy currently means 1,000 / 100. Verify that materialized configs and ready
slots still match, and keep the pins through mixed-version operation and rollback.
Then change one policy at a time after host qualification. Changing its assignment
still invalidates that policy's old slots; larger refill budgets can shorten but
cannot eliminate the cold-create interval and increase refill pressure.

The reconciler retries nominally every five seconds with bounded creation
concurrency and warming budgets. Delete errors can leave unclaimable rows until
retry; missing placement or repeated create failures can prevent refill
indefinitely. Monitor ready/claimable counts, misses, reconciliation failures,
and create/restore tails. Binary rollback or old-controller leadership can
invalidate an inheriting pool again. Raising a default is not a rolling pool
migration; do not rely on spare old slots remaining claimable during refill.

Provision and validate a host budget before increasing a dense deployment's
guest allowance. Upgrade workers before control-plane writers, then verify both
cold creates and warm claims. Existing Firecracker drive APIs accept explicit
limits from new callers, but omitted legacy wire fields resolve at the receiving
binary's default. Do not rewrite every stored 1,000-IOPS value: it may be an
intentional assignment. Existing sandboxes need replacement to receive a new
creation allowance.

`SPACE_DISK_IO_SAFETY_MAX_OPERATIONS_PER_SEC` and
`SPACE_DISK_IO_SAFETY_MAX_BANDWIDTH_MIB_PER_SEC` bound explicit Admin API policy
overrides; they do not cap the platform default, inherited policies, or running
VMs. API startup warns if the configured default exceeds an explicit-policy ceiling. Review those ceilings before upgrading. Without a ceiling
override, explicit policy validation uses the platform default. To retain a
smaller allowance for a workload, assign both policy fields explicitly. A binary
rollback changes future defaults and inherited-policy reconciliation, but does
not lower already persisted assignments or clear the parent cgroup budget.

## Host disk protection

Node-manager can set `io.max` on the sandbox parent cgroup, `/sys/fs/cgroup/space`.
Management processes outside that cgroup do not consume this budget. The feature
is opt-in: an absent `enabled` leaves existing limits untouched; `enabled = false`
clears limits on the currently resolved backing devices. Invalid capacity,
unexpected device count, or failure to write a limit prevents node-manager from
becoming ready. A write failure can leave some devices configured; correct the
configuration and restart, or explicitly disable it. Removing the section alone
does not undo an applied limit.

For storage without a built-in profile, configure measured aggregate capacity in
the node-manager TOML file (`--config`). This synthetic example describes two
equally capable backing devices; replace the capacity values with measurements
from the intended filesystem, read/write mix, and acceptable tail latency:

```toml
[disk_io_reservation]
enabled = true
target_path = "/space/cache"
sandbox_capacity_percent = 70
read_weight_percent = 50
write_weight_percent = 50

[disk_io_reservation.custom_capacity]
expected_device_count = 2
read_iops = 100000
write_iops = 60000
read_bandwidth_mib_per_sec = 1000
write_bandwidth_mib_per_sec = 600
```

This example allows aggregate sandbox reads of 35,000 IOPS and 350 MiB/s,
and writes of 21,000 IOPS and 210 MiB/s. Node-manager divides each aggregate
limit equally across the resolved leaf devices, rounding down. Both weights
must be positive and sum to 100; the sandbox percentage must be in `1..=100`.
The device count is checked before writing any limits. Heterogeneous devices
need a conservative capacity based on the slowest member; capacity percentages
are a budgeting model, not a guarantee of management latency or reserved IOPS.
Measure buffered-write attribution and writeback on the deployed kernel as well
as direct I/O; the VM uses host-buffered disk I/O.

`custom_capacity` is mutually exclusive with `capacity_profile` and makes no
cloud metadata request. Built-in profiles `aws-r6id-metal` and `aws-m5d-metal`
use published saturation capacity and expect four leaf devices. When enabled
without either capacity setting, node-manager selects one of those profiles
through AWS IMDSv2 and fails on unsupported instance types. No host capacity is
assumed for an unconfigured deployment. Deploying the binary alone does not
enable a parent cap.

The parent cap bounds physical I/O reaching its backing devices, including
concurrent guest bursts, but not CPU, memory pressure, or snapshot-memory faults
served from cache. Validate foreground p95/p99 latency under concurrent read and
write load, including after idle guests have accumulated burst credit. Track
physical device I/O, cgroup `io.stat` and pressure, and lifecycle latency alongside
guest throughput. Do not select a budget from device utilization alone.

The same settings are available through `SPACE_NODEMGR_DISK_IO_CAPACITY_PROFILE`,
`SPACE_NODEMGR_DISK_IO_SANDBOX_CAPACITY_PERCENT`,
`SPACE_NODEMGR_DISK_IO_READ_WEIGHT_PERCENT`, and
`SPACE_NODEMGR_DISK_IO_WRITE_WEIGHT_PERCENT`. Environment values override the
corresponding TOML values. `SPACE_NODEMGR_DISK_IO_CUSTOM_CAPACITY` accepts a JSON
object with the five fields in the example's `custom_capacity` section. Unknown
fields, incomplete objects, invalid numbers, and a simultaneous named profile
are rejected. Removing a custom-capacity variable does not clear live limits;
use the explicit disable procedure above.

Only I/O charged to the sandbox cgroup consumes its budget. Filesystem metadata,
transaction commits, and writeback can have different attribution from the
application's data writes. Measure both cgroup and physical device bytes and
requests, including their difference, under buffered writes. Run a foreground
fsync probe outside the capped group on the same filesystem: being outside the
group does not prevent waiting on shared filesystem work. Calibrate against
those latency results rather than treating the uncharged traffic as reserved
management capacity.

## Uploads

Uploads reach the guest's filesystem service as a stream of frames on a gRPC
connection that all of a sandbox's transfers share.

### Where the bytes go

A write that is not an append replaces its destination when it can: a new
file, or an existing regular file that the writing user owns, with one link
and no access ACL, in a directory where the user can create a temporary file.
The bytes go to a temporary file next to the destination, which is renamed
over it when the write completes.

Every other write goes to the destination in place: appends, other files, and
an eligible write whose directory refuses the temporary file. A destination
that cannot be renamed over, such as a bind mount, gets the upload copied into
it in place once the rename fails. None of these is published atomically.

### How the bytes are written

The bytes are staged in physically contiguous memory and written with
`O_DIRECT` wherever the file and its filesystem allow, so each block of data
takes one request instead of one per page.

- **Small uploads** (at most 256 KiB, not appends) are collected in ordinary
  memory, and when the upload finishes are copied into a free upload buffer
  and written direct in one call. They hold no buffer while their frames
  arrive.
- **Larger uploads** are collected in 2 MiB huge-page-aligned upload buffers,
  each written direct once full.
- **Partial blocks.** The final partial block, and anything the filesystem
  refuses to take direct, is written buffered from where the direct writes
  stopped.
- **The buffer pool.** Each filesystem service holds at most eight upload
  buffers (16 MiB). Idle buffers are kept, so the pool keeps its huge pages
  after the guest's memory fragments, and released lazily (`MADV_FREE`), so
  the kernel reclaims them first under memory pressure. Eight buffers keep
  concurrent megabyte uploads at the default bandwidth limit.
- **No waiting for a buffer.** An upload that finds no buffer free writes its
  frames through the page cache as they arrive. Waiting would stop reading
  frames on the connection that every transfer shares.

### Completing, publishing and acknowledging

Every write's data has been written, direct to the virtual disk or into the
guest's page cache, before the write is acknowledged.

- **A replacement** is then renamed over its destination, which publishes it
  atomically: concurrent readers see the previous content (or no file) or the
  new content, never a partial file. Neither the temporary file nor the
  rename is synced.
- **A write in place** is synced (`fsync`) before it is acknowledged. Nothing
  publishes it atomically; the sync puts its bytes on the virtual disk before
  the reply and reports a failed writeback to the writer.
- **A batch write** follows the same rules for each entry.

The public contract (`files:write` in the API reference):

> Creating a file, or overwriting a regular file that the writing user owns
> with no other hard links and no access ACL, replaces the destination with a
> temporary file where it can, which publishes the new content atomically:
> readers see the previous content (or no file) or the new, never a partial
> file. A destination that cannot be renamed over (such as a bind mount), a
> directory that refuses the temporary file, appends and other files are
> written in place, without that guarantee, and synced before the write is
> acknowledged. Acknowledgement does not guarantee that a created or
> overwritten file persists across a sandbox crash, a forced stop or a host
> failure, and a writeback error can surface after it. An ordinary stop
> attempts to flush the sandbox's root filesystem first, but proceeds if that
> attempt fails or times out. No persistence deadline is promised.

What this means for a replacement:

- **Persistence** comes from the guest filesystem's own writeback or a
  lifecycle flush ([Lifecycle boundaries](#lifecycle-boundaries)). Until then
  a guest crash, forced stop or host failure can lose it, leaving the
  destination's previous content, or no new file.
- **Errors.** A direct write allocates its blocks, and ext4 reserves blocks
  for buffered data, when `write` is called, so running out of space fails
  the upload. An I/O error during later writeback does not reach the writer.
- **Crash ordering.** Data written direct is on the virtual disk before the
  rename. For buffered data, ext4's default ordered journaling writes a
  file's allocated data before committing the metadata that exposes it, and
  allocates a file's delayed blocks when it is renamed over an existing one,
  so a crash normally leaves the previous content or the complete
  replacement. A newly created file's buffered part has no such rename
  handling and can be missing after a crash.

Replacements are not synced because a sync forces an ext4 journal commit,
whose 4 KiB blocks are each their own disk request; for small uploads that
costs several times the requests of the data itself, against the drive's
operations limit. To make replacements persistent at acknowledgement
instead, sync the temporary file before the rename (`finalize_written_file`
in `crates/libs/spaced/guest/server/src/filesystem/write.rs`), keep the
destination's directory open from `open_replacement`, and sync it after the
rename; the comment in `Replacement::commit` marks the place.

## Lifecycle boundaries

Persistence is handled where the sandbox's lifecycle changes, not per write.

- **Ordinary stop.** A stop terminates the VM without shutting the guest
  down, so sandbox-manager first asks the guest to `syncfs` its root
  filesystem. A paused VM is resumed for the flush: pause flushes before it
  freezes the VM, and a write still in progress then can complete after that
  flush, in the paused VM's memory. The whole attempt (reading the VM's
  state, resuming it, connecting, and retrying) has one 30 s budget, and
  node-manager bounds each Firecracker pause or resume on its side too; a
  flush already running in the guest may predate the latest writes, so a busy
  guest is retried until this attempt's own flush runs. If the attempt fails
  or times out, the stop proceeds and records the `stop_guest_flush_failed`
  event and the `sandbox.worker.stop.guest_flush` metric. A delete never
  flushes; it discards the disk.
- **Pause and suspend** prepare the guest for a VM snapshot, flushing its
  filesystems first, and fail rather than proceed without it. A successful
  suspend persists what the guest had written, with the snapshot.
- **Disk-only snapshots** flush the guest's filesystems and then the host's
  copy of the image.
- **A crash, forced stop or host failure** can lose writes that the guest had
  not yet written back.

## Reads

A read crosses the data plane, node-api, sandbox-manager, spaced-host, the
guest's vsock connection, the guest router and, for a user other than the
guest daemon's, that user's identity worker. Its cost is CPU per byte at each
hop.

- **Descriptor handoff.** The identity worker opens the file as the user (the
  `OpenRead` call, worker protocol version 3); the router duplicates the
  worker's descriptor with `pidfd_getfd` and streams the file itself, so the
  bytes do not pass through the worker. A worker without `OpenRead`, or a
  descriptor that cannot be duplicated, falls back to relaying the read
  through the worker. The read counts against its worker's per-user
  admission, which ends with the worker, so the read ends with it.
- **Chunks.** Files stream in 64 KiB chunks through a queue of four, so an
  open read buffers at most 256 KiB.
- **Decoding without copies.** `FileChunk.data` is generated as
  `bytes::Bytes`, so each hop's decoded chunk is a slice of the received
  buffer.
- **Frames.** Every SPACE gRPC server, and the file-path clients (data plane
  to node-api, node-api to sandbox-manager, sandbox-manager to spaced-host,
  spaced-host to the guest), advertise a 256 KiB maximum HTTP/2 frame, so a
  chunk crosses each hop in one DATA frame.

## Known limitations

- Journal commits and buffered writeback reach the disk one page per request.
  A device that takes several segments per request would merge them; that is
  a Firecracker change.
- Combined physical disk traffic is unbounded unless a host budget is enabled.
  A parent budget does not establish per-sandbox fairness or bound host-cache I/O.
