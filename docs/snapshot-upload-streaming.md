# Snapshot upload streaming

Volume-manager can feed its zstd encoder directly into the object store's
bounded multipart uploader. For a compressed artifact of size C, this avoids
writing a C-byte temporary file and reading it again for MD5 and upload. It
does not remove source staging, btrfs-send materialization, compression, or S3
payload bytes. Physical device-read savings depend on page-cache hits.

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
Compression and hashing run off Tokio workers. Source cache advice retains the
existing end-of-compression behavior; incremental eviction is separate work.

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
upload or assume regeneration is byte-identical. The queued JSON, SQLite schema,
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
