# Snapshot upload streaming

Volume-manager can feed its zstd encoder directly into the object store's
bounded multipart uploader. For a compressed artifact of size C, this avoids
writing a C-byte temporary file and reading it again for MD5 and upload. It
does not remove source staging, compression, or S3 payload bytes. The separate
btrfs source path below also avoids materializing the uncompressed send stream.
Physical device-read savings depend on page-cache hits.

The deployment setting `SPACE_VOLUME_STREAM_PUSH_BUFFER_BUDGET_BYTES` controls
both mode and aggregate retained-part memory. It is read once at service startup:

| Value | Behavior |
| --- | --- |
| Unset or `0` | File uploads; no streaming producers or part buffers. |
| `1073741824` | Enable eligible streaming uploads with a shared 1 GiB part budget. |
| Other positive values | Enable streaming with that budget; must be whole MiB and at least 64 MiB. Invalid values fail startup. |

The service parses this into `SnapshotUploadMode::File` or a validated streaming
budget. There is no separate boolean that can contradict the budget. Clones
share the same budget across transfers. Part size and request scheduling remain
implementation policy; this setting is not exposed to sandbox users.

Eligible inputs are staged memory, rootfs, btrfs-send, `snapshot.luks`, and
`cow.luks` artifacts of at least 256 MiB, with compression enabled. LUKS inputs
are already encrypted; the same zstd pipeline preserves their encryption format.
Encoded objects smaller than one part use a single PUT after producer success.

Small/metadata objects, legacy unstaged jobs, envelope-encrypted SPACEAEA
artifacts, unsupported object stores, and inputs too large for the fixed part
size retain the file path. File mode remains supported for memory-constrained
deployments and rollback. Smaller positive budgets impose more backpressure;
an in-flight streaming upload never spills to a temporary file when the budget
fills. Changing modes requires restart and the existing drain/cancel handoff.

## Memory and scheduling

One service-owned budget caps retained part allocations at the configured
value, shared across executor clones and transfers. Each part is 64 MiB and each
object has at most
16 concurrent part uploads; the aggregate budget can limit this further. The
budget covers parts while filling, hashing, uploading, and retrying, including
SDK body clones. It is a payload-allocation cap, not a bound on total RSS or
cgroup memory.

The existing total codec limit also caps live streaming producers, including
those waiting on downstream I/O. Each additionally owns an 8 MiB pipe, an 8 MiB
encoder-output buffer, zstd workspace, and small read buffers. Transport/runtime
allocations and filesystem page cache are additional memory. The part budget is
therefore not the service's total memory allowance.

Encoding takes the existing codec admission and honors the priority reserve.
When a pipe write blocks, the producer lends that idle codec slot to queued
work, then reacquires it before returning to zstd. The separate producer permit
stays owned until the blocking task exits, bounding memory even when codec slots
are lent. Inputs below the existing admission threshold retain the file path
and its bypass. Neither codec limits nor the protected reserve are increased.

The producer owns its open source descriptor and producer permit until it exits.
Compression and hashing run off Tokio workers. On Linux, as compression consumes the
immutable staged input, the reader advises away clean pages in completed
64 MiB ranges, then retains the existing whole-file hint after success. Unread
ranges are untouched. This reduces input page-cache residency during large
uploads without changing durable bytes, retry ownership, or the part budget.
The hint is best effort: dirty pages can remain until normal writeback, and
retries may reread evicted input from disk. It is not a total-memory bound and
does not evict restore output or other service caches.

## Ownership and failures

| Outcome | Object-store state | Local ownership and next action |
| --- | --- | --- |
| Success | Encoder finalized, parts completed, HEAD matches compressed length | Report original logical bytes; existing terminal cleanup removes staging. |
| Retriable part error | Retry retains the exact compressed part and MD5 | Same allocation remains charged; producer backpressures when the shared budget fills. |
| Invalid upload configuration or encoded size exceeds the declared bound | Stop producer and abort any incomplete upload | Permanent job failure; retrying the same source and policy cannot fix it. Existing terminal cleanup owns staging. |
| Producer error, cancellation, or upload failure | Abort incomplete multipart upload; never complete after producer failure | Close the pipe, cancel and join producer on ordinary error paths; existing job retry/terminal cleanup rules apply. |
| Executor future dropped on timeout | Best-effort abort runs through the uploader guard | Token/reader drop stops the producer, which retains its descriptor and producer permit until exit. The next attempt rereads staged input. |
| HEAD mismatch or missing object | A completed object may exist but is not accepted as ready | Retain staging while attempts remain; whole-upload retry uses a new upload ID and overwrites the object. |
| Process death | Incomplete S3 upload may survive | Persisted jobs recover through the existing worker; bucket abort-incomplete lifecycle policy is still required. |
| Artifact sibling fails | Successful entries may already exist remotely | Existing artifact-level cleanup retains all retry sources until the whole artifact is terminal. |

Retries regenerate a complete stream. They do not resume an old multipart
upload or assume regeneration is byte-identical. For file-source uploads, the queued JSON,
SQLite schema,
protobuf API, object keys, and zstd/LUKS formats are unchanged. The existing
consumer can read streamed objects.

Rollback sets the budget to zero for the successor process and uses the
existing drain/cancel handoff. Normal cancellation joins the producer; an
executor timeout uses the drop path above. Changing configuration is not an
instantaneous in-flight switch.
Recoverable staged jobs then use file uploads.

## Measurements

File-backed `volume_manager.push.*` sizing counters retain their existing
semantics: transformed `prepared` wire bytes size completed temporary files.
Streaming instead emits `volume_manager.push.stream.payloads`, `logical_bytes`,
and `wire_bytes`, with `artifact` and `phase` tags. `uploaded` means successful
producer finalization and object completion; `verified` additionally passed
HEAD. Each phase counts attempts, not unique artifacts or retransmitted network
bytes. Sum file and stream **verified** counters for accepted uploads; never
sum phases or count streaming wire bytes as temporary disk writes.

`volume_manager.push.stream.producer_ms` measures admitted producer lifetime.
`pipe_write_ms` includes pipe backpressure, bridge overhead and codec
reacquisition, not CPU time.
Both have bounded `artifact` and `outcome` tags. Compare these with codec wait,
restore latency, cgroup anonymous/file memory, PSI, CPU, and device I/O under
matched concurrency. Decoding synthetic ciphertext bytes verifies transport
integrity; it does not prove an encrypted sandbox boots after restore.

Before measuring a canary, verify that the telemetry backend retains `artifact`
and `phase` on all three streaming counters, `artifact` and `outcome` on both
producer distributions, and deployment/node dimensions needed for comparison.
Enable distribution percentiles before interpreting p50/p99. If metric policy
can only be configured after ingestion, use a limited validation upload to
bootstrap it, verify the policy, then start a fresh measurement window. Missing
or dropped tags are not evidence of zero traffic or zero latency.

Use the same budget setting for ordinary and LUKS artifacts. The encoder test
compares uploaded bytes directly with `compress_to_temp` for sparse inputs of
every eligible artifact type. Before widening the canary, also suspend and
restore a real encrypted sandbox through the remote uploaded artifacts, with
both `snapshot.luks` and `cow.luks` selected for streaming. Verify guest data and
successful VM execution; a local-cache wake or synthetic ciphertext round trip
does not establish this. Confirm abort-incomplete lifecycle cleanup on the
actual snapshot bucket used by that canary, including non-production buckets.

## Streaming from retained btrfs sources

`SPACE_SNAPSHOT_STREAM_BTRFS_SEND=true` on sandbox-manager opts new btrfs
captures into retaining an immutable `rootfs.btrfs-source` bundle. It defers
send-file creation only when volume-manager advertises streaming support
(compression enabled, a positive upload buffer budget, and an authorized
node-manager capability probe). The default remains
the existing materialized path. LUKS artifacts keep their existing upload path;
this option changes the btrfs-send codec, not encryption policy.

Each bundle contains private copy-on-write snapshots of the frozen rootfs and
its template parent, plus a manifest binding their read-only subvolume UUIDs to
one source identity. Template GC and later sandbox writes cannot change them.
These copies share extents but can delay physical reclamation until artifact
retirement. They are not copies of the entire rootfs written to disk.

Before acknowledging submission, node-manager creates an independent upload
lease under `/space/cache/.btrfs-send-staging`. Volume-manager persists that lease
in a `push_btrfs_send` job. Sandbox teardown can then remove the original artifact
without breaking upload retries. No SQLite migration is needed: the source
reference lives in the existing job payload. The new protobuf RPCs are additive.

For each attempt, node-manager creates a fresh wire clone, sends into a kernel
pipe, and relays bounded chunks over its existing Unix gRPC socket. Volume-manager
compresses the stream into the same shared multipart budget used by other pushes.
Node-manager permits four concurrent send readers independently of its filesystem
mutation lane. Each relay queues at most four 256 KiB data frames, plus its pipe,
current frame and gRPC transport buffers. Filesystem cache remains additional
memory. Volume-manager retains its existing codec reserve and producer limit.

Completion requires a successful kernel send, an exact terminal byte count,
successful gRPC trailers, encoder finalization, and an S3 HEAD length check.
Retries start a new send and multipart upload; regenerated streams need not be
byte-identical. The remote zstd/btrfs-send format and restore decoder do not change.
This removes one uncompressed send-file write and its compression read. It does
not eliminate reads of the source rootfs or reduce S3 bytes.

The logical send length is unknown until encoding finishes. Sandbox-manager
therefore submits remote metadata after all VM artifacts complete, writes it
outside the reclaimable artifact directory, and persists the metadata job ID
before polling it. Ready publication still waits for every artifact and metadata.
The local sidecar retains source identity for recovery; the remote sidecar records
the actual logical upload total and omits that node-local identity.

| State or failure | Owner and recovery |
| --- | --- |
| Capture interrupted | The artifact owner reclaims partial subvolumes; no complete source identity is published. |
| Staging succeeds but job insertion/reply fails | Submission and the orphan census serialize. A retry rejoins the same durable job when present; an unreferenced lease is reclaimed by the next census. |
| Upload pending, running, or cancelling | The persisted volume job retains its lease. Retries reopen and verify immutable inputs. |
| Reader disconnect, timeout, or daemon shutdown | Close the pipe read end, join the kernel writer, then release its lease lock and reader slot. A retry generates a fresh stream. |
| Job commits completed, failed, or cancelled | The one-minute orphan sweep can release its lease. It cannot race an active kernel reader because deletion takes the same lease lock. |
| Census incomplete or malformed payload | Delete nothing; log and retry next minute. Each sweep is bounded to 30 seconds. |
| Snapshot superseded or source identity changed | Reject reuse of the replacement source; existing job retry/failure convergence owns the outcome. |
| Upload budget becomes zero | Existing source jobs compress into a temporary file and use the supported file uploader. New captures materialize their send file. |
| Metadata submission or finalization interrupted | Recover persisted VM/metadata job IDs and retry the immutable backup attempt; do not publish Ready early. |

Deploy node-manager and the volume-manager identity update first: the latter adds
socket access through `space-nodeclients`, while per-operation authorization grants
only stage/read/list/release send-source operations. Then deploy volume-manager
and sandbox-manager, and opt in new capture after validation. Old servers retain
the file path when capability negotiation is unavailable. Deployments that still
share sandbox-manager's UID with volume-manager must migrate to the dedicated
volume-manager identity before enabling this path; negotiation otherwise keeps
the materialized file. The capability probe adds at most two seconds before
falling back if a service is unavailable.

Turning the capture option off stops new deferred captures. Setting the upload
budget to zero changes how existing source jobs upload; it does not remove their
source dependencies. Before downgrading binaries, drain source jobs and materialize
any retained local artifacts needed by old readers using `BtrfsMaterializeRootfsSend`.
Do not downgrade node-manager while source-aware volume jobs remain. Source cleanup
continues while streaming is disabled. Normal artifact retirement reclaims the
original bundle; the upload lease has independent terminal cleanup.

### Isolated btrfs transfer measurement

An optimized Linux harness compared four concurrent 4 GiB synthetic rootfs
sends using the existing staged-file upload path and the new direct-source path.
Both used the same uploader, four codec slots, and a 1 GiB part budget. Separate
cgroups limited the source service to 2 GiB and the upload executor to 12 GiB.
Four interleaved batches per path produced these medians:

| Per 16 GiB input batch | Materialized send → streamed upload | Direct send → streamed upload |
| --- | ---: | ---: |
| Send preparation plus upload | 19.1 s | 10.5 s (45% lower) |
| Device reads across both cgroups | 30.2 GiB | 16.0 GiB (47% lower) |
| Device writes across both cgroups | 16.01 GiB | <1 MiB |
| Total CPU time | 53.5 CPU-s | 71.4 CPU-s (34% higher) |
| S3 object bytes | 4.01 GiB | 4.01 GiB |

All 32 objects were downloaded, decoded, received by btrfs, and rootfs-hash
verified. Four further uploads verified budget-zero fallback. Median peak
anonymous memory was 10.3 → 19.8 MiB in the source service and 904 → 734 MiB in
the uploader; the configured part cap remained unchanged. Neither cgroup OOMed.
These are synthetic transfer results, not fleet percentages or VM restore-tail
measurements. Source capture/staging happened before the measured transfer;
retained COW extents can add a capacity cost until their owner retires them.

A separate transport-only probe drained the same send bytes without compression
or S3: an FD-passing prototype took 4.94 s and 24.5 CPU-s, versus 6.65 s and
48.6 CPU-s for the gRPC relay (three batches each). The relay therefore has a
measured CPU cost. This implementation keeps the existing authenticated RPC
transport and node-owned read end, which lets node-manager stop a backpressured
kernel writer when its RPC consumer disappears. FD passing would require a
separate authenticated handoff protocol and cancellation rules for a read end
owned by another process. It remains a concrete CPU optimization opportunity;
its transport-only result is not an end-to-end S3 upload measurement.
