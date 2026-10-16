# RustFS vs versitygw for Kind with the storage exchange on

## Verdict: GO WITH CONDITIONS

RustFS 1.0.1 can be Kind's default object store with credential-manager's
storage exchange on, under one condition:

1. **The Kind chart raises RustFS's large-write admission limit**, for
   example `RUSTFS_PUT_LARGE_FOREGROUND_ADMISSION_LIMIT=64`. Check: `perf.sh
   rustfs` with that setting reports `failed uploads | 0 of 216`, as measured
   here ([`d-variant-rustfs-limit64.md`](results/d-variant-rustfs-limit64.md)).

   Stock RustFS admits at most 32 concurrent large PUTs. Under #109's burst
   of 36 uploads of 128 MiB, it answers the rest with `503 SlowDown` before
   reading their bodies. SPACE's object-store client then gives up on 4 of
   the 36 uploads in every run; versitygw loses none. Raising the wait
   instead of the limit trades the 503s for the client's own stall timeout.
   With the limit at 64, all three bursts stored every object. The median
   phase was 1.26× versitygw's (the bound is 1.5×), with no zero-progress
   stall.

All the other listed criteria hold:

- **A, B and C pass.** RustFS handles every S3 behaviour SPACE uses. It
  refuses a mismatched `Content-MD5` with `400 BadDigest` and stores nothing.
  Freezing it for 7, 17 or 25 s mid-upload never left a short or wrong
  object. It enforced the authority's exact-object and prefix session
  policies on 47 of 47 checks, refused expired sessions, and refused forged
  tokens. Role validation is not counted (the brief excepts it): RustFS
  accepts any `RoleArn`. versitygw fails C outright, because it has no
  `AssumeRole`.
- **D, as the rule states it, holds even on stock settings.**
  - Start → ready is 0.26–0.63 s.
  - The burst's median phase is 14.99 s against versitygw's 17.68 s, but
    that phase includes the 4 uploads that failed fast. With the condition
    applied, all uploads succeed and the median is 22.26 s.
  - There are no zero-progress stalls of 5 s or more.
- **E holds.**
  - Both exchange-mode suites on fresh clusters passed every scenario except
    one. That scenario, `local.ecw_template_build_from_oci_boots`, is
    ENVIRONMENTAL: the builder's storage admission wants 52 GiB free on this
    box's Kind disk and found 22.7–33.0 GiB. It passes in Main CI and fails the
    same way on the versitygw baseline.
  - The fault scenarios (pending-backup census, and native recovery with the
    store scaled to 0) passed in both runs.
  - The negative proof through the node's real agent path held on every row.
  - The only transfers over 5 s are four 16 GiB `snapshot.luks` pushes per
    run (10.3–10.7 s), from the two encrypted-snapshot scenarios. The
    versitygw baseline shows the same four pushes, at 10.40–10.70 s.

The condition is a Kind chart setting, not a RustFS fix, so it is checkable
on this branch. The branch does not set it yet, so its chart is the one the
Kind runs tested. Those suites never sent more than 32 large PUTs at once:
run 2's store log shows no 5xx. Making exchange mode the default also needs
the RustFS image in `ci/mirrored-images.txt`, as versitygw is. That is a
delivery step, not a criterion.

## A. S3 behaviours SPACE uses

Probe `behaviours`, run against each store on the same box. Rows that name the
client (put_bytes, put_file, get_file, put_stream, the bad-secret HeadObject)
drive `crates/libs/object-store`; the rest use the AWS SDK for requests the
client never makes. Full tables: [`a-behaviours-rustfs.md`](results/a-behaviours-rustfs.md),
[`a-behaviours-versitygw.md`](results/a-behaviours-versitygw.md).

| Behaviour | RustFS 1.0.1 | versitygw v1.8.0 |
|---|---|---|
| put_bytes + HeadObject; HeadObject on a missing key → `None` | PASS | PASS |
| put_file 40 MiB multipart (Create, UploadPart ×5, Complete; UNSIGNED-PAYLOAD + Content-MD5) | PASS | PASS |
| get_file: ranged GETs with If-Match, SHA-256 identical | PASS | PASS |
| Range `bytes=100-199` + If-Match current ETag → 206, exact bytes | PASS | PASS |
| Range + If-Match stale ETag → 412 | PASS | PASS |
| put_stream 20 MiB in 5 MiB parts; below one part (single PUT) | PASS | PASS |
| Create, UploadPart, Abort: upload and object gone | PASS | PASS |
| UploadPartCopy ×2 with `x-amz-copy-source-range`, Complete | PASS | PASS |
| ListObjectsV2 max-keys 10 over 25 keys: 3 pages, ordered | PASS | PASS |
| DeleteObjects (25 keys) | PASS | PASS |
| Bad SigV4 secret refused (Head 403; Put 403 SignatureDoesNotMatch); nothing stored | PASS | PASS |
| Unknown access key refused | 403 InvalidAccessKeyId | 404 XAdminUserNotFound (refused, non-AWS code) |
| PutObject `UNSIGNED-PAYLOAD` + Content-MD5 | PASS | PASS |
| Info: `STREAMING-UNSIGNED-PAYLOAD-TRAILER` with CRC32 trailer (SPACE turns trailers off) | 200, identical | 400 InvalidRequest |
| **Total** | **17/17** | **17/17** |

## B. Integrity

### B1. Mismatched Content-MD5

| Check | RustFS | versitygw |
|---|---|---|
| PutObject (UNSIGNED-PAYLOAD), wrong Content-MD5 → 400, nothing stored | 400 BadDigest, 404 after | 400 BadDigest, 404 after |
| PutObject (signed payload), wrong Content-MD5 → 400, nothing stored | 400 BadDigest, 404 after | 400 BadDigest, 404 after |
| Overwrite with wrong Content-MD5 → 400, original unchanged | PASS | PASS |
| UploadPart, wrong Content-MD5 → 400; ListParts shows only the good part | 400 BadDigest; `[1]` | 400 BadDigest; `[1]` |
| Complete naming the refused part → refused, no object | 400 InvalidPart | 400 EntityTooSmall |
| **Total** | **8/8** | **8/8** |

### B2. Store frozen mid-upload

`docker pause` 2 s into a 200 MiB upload through the object-store client,
with ingress capped so the upload takes about 7 s. The store's own log of every
attempt is in [`b2-freeze-rustfs.md`](results/b2-freeze-rustfs.md) and
[`b2-freeze-versitygw.md`](results/b2-freeze-versitygw.md).

| Upload, freeze | Client (RustFS) | Stored (RustFS) | Client (versitygw) | Stored (versitygw) |
|---|---|---|---|---|
| put_file single 200 MiB, 7 s | Ok after 16.9 s, 1 stalled-stream retry | identical (size, SHA-256) | Ok after 16.7 s, 1 retry | identical |
| put_file single, 17 s | Ok after 26.9 s, 2 retries | identical | Ok after 26.7 s, 2 retries | identical |
| put_file single, 25 s | Err: throughput 0 B/s after retries | absent | Err: same | absent |
| multipart 200 MiB (4 parts), 7 s | Ok after 16.5 s, 3 part retries | identical | Ok after 16.8 s, 4 part retries | identical |
| multipart, 17 s | Ok after 26.6 s, 6 part retries | identical | Ok after 26.8 s, 8 part retries | identical |
| multipart, 25 s | Err: throughput 0 B/s | absent (upload not completed) | Err: same | absent |
| **Pass: never a short or wrong object** | **6/6** | | **6/6** | |

Each abandoned attempt was rejected by the store. RustFS logs it as
`400 IncompleteBody` (its own length check), and versitygw as `400 BadDigest`
(the Content-MD5 check). versitygw v1.8.0 has the upstream bug #108 guards
against (versity/versitygw#2414: a short non-multipart PUT is published as
complete; fixed in v1.9.0), so for versitygw Content-MD5 is what holds this
row. RustFS holds it twice over.

## C. The exchange's requirements

Probe `exchange` (RustFS: the vend identity and its policy created through
RustFS's admin API, as the chart's bucket-init Job does) and probe `expiry`.
The session policies are the authority's exact shapes
(`storage_exchange.rs`): exact object, and prefix with `StringLike`
`prefix` and `prefix/*`. Full tables: [`c-exchange-rustfs.md`](results/c-exchange-rustfs.md),
[`c-expiry-rustfs.md`](results/c-expiry-rustfs.md), [`c-exchange-versitygw.md`](results/c-exchange-versitygw.md).

| Requirement | RustFS 1.0.1 | versitygw v1.8.0 |
|---|---|---|
| AssumeRole with `Policy` and `DurationSeconds` | PASS (3600 s session) | **FAIL: 405, no AssumeRole** |
| Exact-object resource: multipart put_file and put_stream, HeadObject, ranged get_file allowed | PASS | n/a |
| ListBucket with `s3:prefix` `StringEquals` the key allowed; the sandbox directory, no prefix, another key refused | PASS | n/a |
| Multipart Create/UploadPart/Complete/Abort under `s3:PutObject` | PASS | n/a |
| HeadObject under `s3:GetObject` | PASS | n/a |
| Read-only session: Put, multipart, CreateMultipartUpload refused; object unchanged | PASS | n/a |
| Another object, a key extending the name, a key under it: refused (read and write) | PASS | n/a |
| Same key in the other vended bucket refused; DeleteObject (not granted) refused; CopyObject from another sandbox refused | PASS | n/a |
| Prefix session: writes and lists under `prefix` and `prefix/`, nested keys allowed; `prefix` + `x`, sibling sharing the string, the prefix as a key, other sandbox, other bucket refused | PASS | n/a |
| Expired session refused | PASS: shortest accepted `DurationSeconds` is 900 (1 and 60 are silently raised to 900, where AWS rejects them); allowed 60 s before expiry, refused 30 s after | n/a |
| Forged or altered token refused: last character changed, widened `sessionPolicy` claim, JWT re-signed with the vend secret, keys without the token, keys with another session's token | PASS, but with **HTTP 500**, not AWS's 403 | n/a |
| Vend identity's own keys outside its policy (other bucket) refused | PASS | n/a |
| Role validation (recorded only) | Not validated: a role naming no role, one in another account, a malformed ARN and no `RoleArn` are all accepted | No AssumeRole |
| **Total** | **47/47 + expiry 4/4** (6 informational) | **0/1** |

## D. Start-up and performance

`perf.sh`, one store at a time on the same box, each with the Kind chart's
limits (1 CPU, 4 GiB), a fresh store per burst. The burst is 36 concurrent
128 MiB uploads, first through the object-store client (`put_file`), then
through the AWS SDK with timed bodies (ACK gap and zero-progress windows),
three times. Full tables: [`d-perf-rustfs.md`](results/d-perf-rustfs.md),
[`d-perf-versitygw.md`](results/d-perf-versitygw.md),
[`d-variant-rustfs-limit64.md`](results/d-variant-rustfs-limit64.md).

| Measure | RustFS 1.0.1 (stock) | RustFS, admission limit 64 | versitygw v1.8.0 |
|---|---|---|---|
| Start → ready, fresh data (3 starts) | 0.26–0.52 s | 0.63 s (1 start) | 0.18–0.39 s |
| Start → ready, restart on data | 0.36 s | 0.17 s | 0.33 s |
| First write to a new bucket (median of 5) | 11.1 ms | 11.6 ms | 0.7 ms |
| Burst phase, median / worst of 3 | 14.99 s / 20.19 s, **4 of 36 uploads failed every run** | **22.26 s / 23.06 s** (1.26× / 1.30×) | 17.68 s / 17.69 s |
| Slowest PUT, median / worst | 14.99 s / 20.19 s | 22.26 s / 23.06 s | 17.68 s / 17.69 s |
| Longest ACK gap (body sent → response), median / worst | 9.13 s / 9.63 s | 10.40 s / 12.23 s | 1.95 s / 2.04 s |
| Zero-progress stalls ≥ 5 s with a body still sending | 0 | 0 | 0 |
| Failed uploads (both phases, 3 runs) | **24 of 216** (`503 SlowDown`) | **0 of 216** | 0 of 216 |
| Idle memory / CPU | 19.7 MiB / 0.0 % | 20.1 MiB / 0.0 % | 8.8 MiB / 0.0 % |
| Memory during bursts, median / peak | 409 / 933 MiB | 620 / 1271 MiB | 285 / 341 MiB |
| CPU during bursts, median / peak (% of one core) | 99.8 / 106.5 | 99.6 / 107.0 | 100.8 / 108.1 |
| Image (amd64), compressed / unpacked | 111 MB / 404 MB | same | 31 MB / 101 MB |

Other RustFS settings tried (raw tables in `results/d-variant-*.md`; their
stall measure predates the narrower one above):

- `RUSTFS_PUT_LARGE_FOREGROUND_ADMISSION_WAIT_TIMEOUT_MS=60000`: uploads wait
  for admission before sending a body, so the client's stall protection
  fires. 3 failed uploads per run, zero-progress windows over 5 s.
- `RUSTFS_PUT_LARGE_FOREGROUND_ADMISSION_ENABLE=false`: no failures, but
  zero-progress windows of 8 s.
- 4 CPUs: the same 4 failures per run; phase 10.3 s against versitygw's
  8.2 s on 4 CPUs.

The limit of 32 comes from RustFS 1.0.1's defaults
(`crates/config/src/constants/object.rs`): large-write admission is on by
default, it derives at most 32 slots, and a direct PUT waits only 250 ms for
one before `503 SlowDown`. RustFS also frees a deleted object's space only on
a five-minute cleanup pass (it moves the data to `.rustfs.sys/tmp` first), so
back-to-back bursts on one store filled this box's Docker disk. Each burst
above ran on a fresh store for that reason.

## E. On Kind

Settings as `.github/workflows/kind-ci.yml` uses them:

- `SPACE_KIND_TELEMETRY_MODE=otel` and `DATA_PLANE_URL=http://127.0.0.1:8090`;
- the admin token file and cold-receipts directory, made as CI makes them;
- `scripts/tests/smoke-kind-e2e.sh`, then the health gate, then
  `SKIP_BOOTSTRAP=1 scripts/tests/smoke-kind-typed.sh all` (unsharded).

The health gate requires:

- Tilt quiet for 60 s;
- no ReplicaSet younger than 180 s;
- every node Ready and published (`wait-node-publication.sh`);
- then a 120 s settle.

Every run was on a freshly created cluster, on one Linux/KVM devbox. Raw
summaries: [`results/kind/`](results/kind).

| Run | Store, credentials | Bring-up + baseline smoke | ECW pass | Native pass | Not passed |
|---|---|---|---|---|---|
| exchange-1 | RustFS 1.0.1, exchange | PASS (403 s) | 77 passed, 1 infra error | 24/24 passed | `local.ecw_template_build_from_oci_boots`: ENVIRONMENTAL |
| exchange-2 | RustFS 1.0.1, exchange | PASS (347 s) | 77 passed, 1 infra error | 24/24 passed | `local.ecw_template_build_from_oci_boots`: ENVIRONMENTAL |
| baseline | versitygw v1.8.0, static | PASS (356 s) | 77 passed, 1 infra error | 24/24 passed | `local.ecw_template_build_from_oci_boots`: ENVIRONMENTAL (same error) |

**The one failure, classified ENVIRONMENTAL.** `local.ecw_template_build_from_oci_boots`
ends with `BuildException: storage admission requires 55834574848 available
bytes but found 24368959488` (run 1; 35439083520 in run 2). The builder
admits an OCI build only with 52 GiB free on its node's disk. This box's
Kind data disk had 22.7–33.0 GiB free; the rest held the Kind nodes'
containerd data, which this task may not prune. The scenario passed in the
latest Main CI run (2026-10-16, shard 2/4) and failed the same way on the
versitygw baseline here (35197800448 bytes found). It does not touch the
store before admission.

**Fault scenarios.** All passed in both exchange runs:

- `local.pending_backup_census_reports_blocked_upload`, which scales the store
  to 0 under a pending upload;
- `native_recovery.slow_operation_does_not_block_later_work`, which stops
  object storage;
- `native_recovery.command_survives_data_plane_restart`;
- `native_recovery.queued_create_survives_api_restart`.

Every volume-manager job failure in the two runs falls inside a store-down
window: 8 failed attempts of 5 jobs in each run, and every one of those jobs
completed on a later attempt. Each attempt failed with `credential exchange
failed: … failed to assume the configured storage role`. With the exchange
on, STS lives on the store's endpoint, so an outage fails `AssumeRole` first.
On the versitygw baseline the same windows produce 7 failed attempts, as S3
`Connection refused`.

**Negative proof through the node's real agent path**
([`negative-proof.md`](results/kind/exchange-1/negative-proof.md), run 1's
cluster after its suite).

- Setup:
  - Two sandboxes, A and B, were created and suspended.
  - Inside a sandbox node's volume-manager container, running as
    volume-manager's own uid and groups, a static `grpcurl` called the node
    credential-manager agent's volume socket. It sent the same
    `ExchangeToken` request `credentials.rs` sends: agent → authority → STS
    `AssumeRole` → RustFS.
  - The aws CLI in the same container then used each issued credential.
  - This ran on both sandbox nodes. All 25 checked rows held; 2 more are
    recorded only (which node may write).
- Allowed:
  - A read session for A's `memfile` could Head, ranged-Get and List
    exactly that key.
  - A write session for A's snapshot prefix could write under it.
- Refused by RustFS (`AccessDenied`):
  - the read session against B's `memfile` (Head and Get), B's snapshot
    prefix (List), A's other files (`binfile`), and a write;
  - the write session for a put under B's prefix or over B's `memfile`, a
    multipart create under B's prefix, and a put in the templates bucket.
- Refused by the authority (`PermissionDenied`):
  - A's subject naming B's `memfile`;
  - a write session for A requested from the node that does not host A.
    Reads are allowed from any node, by design.

**Snapshot transfer durations** (volume-manager's `job completed`
`elapsed_ms`, per artifact job, subject `sandbox:`; template pulls listed for
completeness):

| Run | Kind | Transfers | p50 | p95 | max | > 5 s |
|---|---|---|---|---|---|---|
| exchange-1 | push (upload) | 155 | 0.06 s | 1.24 s | 10.65 s | 4 |
| exchange-1 | push_btrfs_send (upload) | 47 | 0.07 s | 0.08 s | 0.18 s | 0 |
| exchange-1 | pull (download) | 94 | 0.05 s | 0.36 s | 0.46 s | 0 |
| exchange-1 | template pull | 27 | 0.08 s | 0.90 s | 1.10 s | 0 |
| exchange-2 | push (upload) | 149 | 0.07 s | 1.20 s | 10.55 s | 4 |
| exchange-2 | push_btrfs_send (upload) | 45 | 0.07 s | 0.09 s | 3.76 s | 0 |
| exchange-2 | pull (download) | 94 | 0.05 s | 0.38 s | 0.47 s | 0 |
| exchange-2 | template pull | 27 | 0.15 s | 0.85 s | 0.88 s | 0 |
| baseline (versitygw) | push (upload) | 149 | 0.03 s | 1.18 s | 10.70 s | 4 |
| baseline (versitygw) | push_btrfs_send (upload) | 45 | 0.05 s | 0.06 s | 2.94 s | 0 |
| baseline (versitygw) | pull (download) | 94 | 0.03 s | 0.37 s | 0.38 s | 0 |
| baseline (versitygw) | template pull | 24 | 0.05 s | 0.58 s | 0.59 s | 0 |

Transfers over 5 s:

- Exchange runs: four per run, all `push` of a 16 GiB `snapshot.luks`. Run 1
  took 10.31, 10.57, 10.57 and 10.65 s; run 2 took 10.37, 10.51, 10.52 and
  10.55 s. They come from `local.encrypted_resuspend_preserves_cow` and
  `native.encrypted_double_restore_retires_prior_pods`.
- Baseline: the same four 16 GiB `snapshot.luks` pushes, at 10.40, 10.43,
  10.50 and 10.70 s.

So RustFS shows no transfer over 5 s that versitygw does not. Small transfers
are slower under RustFS: a p50 of 0.05–0.07 s against versitygw's 0.03–0.05 s.
Template pulls' p95 is 0.85–0.90 s against 0.58 s, and the other p95s are
within 0.06 s.

Store side (run 2, from RustFS's request log). Run 1's log was lost when a
scenario replaced the store pod, which is why the follower was added.
versitygw v1.8.0 logs no per-request line by default.

- 1,212 requests, none answered with a 5xx;
- no 403, because the suite never asked for anything outside a session's
  scope;
- in Kind mid-suite, RustFS held about 53 MiB of anonymous memory, with its
  cgroup at about 990 MiB, mostly page cache, under a 4 GiB limit.

## F. Maintenance

| | RustFS | versitygw |
|---|---|---|
| License | Apache-2.0 | Apache-2.0 |
| Image | `docker.io/rustfs/rustfs:1.0.1@sha256:1803faef57627e2d9c2e7d89d655d712ddded5389040054987163043fecb6a3c`, the project's own Docker Hub repository (its README's install path), built by its `docker.yml` workflow with provenance and SBOM attestations | `ghcr.io/versity/versitygw:v1.8.0@sha256:30292fc2eeacc67a36993b01f7a7a5e3361a19cced0e80c1d71cfa2a4b0a2499`, the project's GHCR |
| Release cadence | 1.0.0 on 2026-09-16, 1.0.1 on 2026-10-03; before 1.0, 2–10 prereleases a month through 2026 | Roughly monthly: v1.6.0 (06-26), v1.7.0 (07-15), v1.8.0 (09-04), v1.9.0 (10-13) |
| Published advisories affecting the pinned version | None: all 35 published RustFS advisories, including the policy-evaluation ones (GHSA-6r96-hmgc-726c, GHSA-v9cp-qfw9-9pfp), are patched in 1.0.0 or earlier | GHSA-7qcr-88c3-f9p7 (high, path traversal via uploadId and batch-delete keys) and GHSA-5gjv-3vhq-4chv (medium, unread aws-chunked tail), both fixed in v1.9.0 |
| Open upstream issues on data loss, truncation or policy bypass | None found on truncated or partial objects or policy bypass. Open and relevant to Kind's single-disk layout: [rustfs/rustfs#8015](https://github.com/rustfs/rustfs/issues/8015) (scanner degrades foreground latency), [#8377](https://github.com/rustfs/rustfs/issues/8377) (background self-reads with no client traffic), [#8121](https://github.com/rustfs/rustfs/issues/8121) (memory ratchet, OOM after ~7 days). Not applicable: [#8564](https://github.com/rustfs/rustfs/issues/8564) (FileCorrupt during pool decommission) | Closed in v1.9.0: [versity/versitygw#2414](https://github.com/versity/versitygw/issues/2414) (short non-multipart PUT published as complete), which Kind's mirror list already tracks |

## The exchange path: findings

1. **A store outage surfaces as an STS failure.** With STS on the store's
   endpoint, a store scaled to 0 makes every exchange fail with
   `FailedPrecondition` `failed to assume the configured storage role`. The
   authority's message names the role, though the role was never the
   problem. volume-manager treats it as retriable and the jobs recovered.
   Deployments have a separate STS endpoint, so this coupling is Kind's. The
   message would mislead anyone diagnosing an STS outage.
2. **RustFS answers forged session tokens with 500, not 403.** The request
   is refused, so C passes. A client that retries 5xx (the SDK does) retries
   a forged or corrupted credential before failing. Expired sessions get 403
   as AWS does.
3. **RustFS raises `DurationSeconds` below 900 to 900** where AWS rejects
   it. The authority asks for 3600, so this changes nothing today.
4. **RustFS accepts any `RoleArn`, even a malformed one or none**, not only
   any well-formed one. The authority validates the ARN's syntax first
   (`iam_role_arn.rs`), so Kind only ever sends well-formed ones. The Kind
   README and design note said "any well-formed `RoleArn`"; both now say
   this.
5. **The authority's node check works as the design note describes.** A
   write session for a sandbox was refused from a node not attributed to
   it. A read session was issued from either node.
6. **Earlier correction, kept:** the design note had claimed volume-manager
   always passes `is_prefix = false`. Prefix sessions (`is_prefix = true`,
   `executor.rs`) date back to the initial release; the note was corrected
   on this branch before the bake-off ran C against both session shapes.
7. **Design note figures for RustFS.** Its candidate table, kept as written,
   gives "~150 MiB / seconds" for idle memory and readiness. Measured here:
   20 MiB idle standalone, about 53 MiB anonymous in Kind mid-suite, and
   ready in under 0.7 s.

## What the branch changes

Rebased #478 (its two commits) onto `origin/main` at `a621b08e5`, then:

- `infra/space/kind/charts/space-object-store/` (`values.yaml`,
  `templates/deployment.yaml`, `templates/secret.yaml`,
  `templates/bucket-init-job.yaml`, `Chart.yaml`): in exchange mode, run
  RustFS 1.0.1 pinned by digest in place of SeaweedFS. The bucket-init Job
  creates the vend identity and its user policy (`snapshots` and `templates`
  only) through RustFS's admin API, and the admin keys stay for the Jobs
  and services that still use them. Static mode (versitygw) is unchanged.
- `space-credmgr-authority-local` (values and deployment): the vend role
  ARN, the vend identity's keys and `AWS_ENDPOINT_URL_STS` in exchange mode.
- `space-volume-manager-local` and the credential-manager agent's daemonset:
  sandbox workers get no store keys in exchange mode.
- `Tiltfile`, `infra/space/kind/lib/storage-credentials.sh`,
  `scripts/bootstrap.sh`, `scripts/check-object-store-transition.sh`: the
  mode switch, and the transition check that refuses switching an existing
  cluster between stores.
- `infra/space/kind/README.md`, `docs/design/kind-storage-exchange.md`: the
  exchange mode, the store evaluation, and the corrections above.
- `scratch/rustfs-bakeoff/`: the tools, raw results and this report.

## How to reproduce

Standalone criteria (A–D), from `scratch/rustfs-bakeoff/`; build the probe as
[README.md](README.md) shows, then:

| Criterion | Command |
|---|---|
| Start both stores | `./stores.sh up rustfs && ./stores.sh vend rustfs && ./stores.sh up versitygw` |
| A | `eval "$(./stores.sh env <store>)"; $PROBE --label <store> behaviours` |
| B1 | `$PROBE --label <store> digest` |
| B2 | `./stores.sh throttle <store> 240mbit; $PROBE --label <store> freeze --container bakeoff-<store>` |
| C | `$PROBE --label <store> exchange`; expiry (about 16 min): `$PROBE --label rustfs expiry` |
| D | `PROBE=$PROBE ./perf.sh <store>`; the condition: `BAKEOFF_RUSTFS_ENV=RUSTFS_PUT_LARGE_FOREGROUND_ADMISSION_LIMIT=64 PROBE=$PROBE ./perf.sh rustfs`; afterwards `./stores.sh down <store>` |

Kind (E), from the repository root on a Linux/KVM box with no Kind cluster:

| Run | Command |
|---|---|
| Exchange suite | `SPACE_KIND_STORAGE_CREDENTIALS=exchange bash scratch/rustfs-bakeoff/kind/run-suite.sh exchange-1` |
| Baseline suite | `bash scratch/rustfs-bakeoff/kind/run-suite.sh baseline-static` |
| Negative proof | `bash scratch/rustfs-bakeoff/kind/negative-proof.sh $BAKEOFF_KIND_OUT/exchange-1/admin-token` |
| Fresh cluster between runs | `bash infra/space/kind/scripts/teardown.sh` |

`run-suite.sh` writes everything under `$BAKEOFF_KIND_OUT/<label>`:

- the logs;
- `suite.md`, from `suite-summary.py`;
- `transfer-durations.md`, from `transfer-durations.py` over
  `collect-volmgr-logs.sh`'s output;
- `store-requests.md`, from `store-log-summary.py` over
  `collect-store-logs.sh`'s output.

## How these runs were done on this box

- **Rust artifacts.** The Docker disk (49 GB) could not hold the Rust build
  cache and the runtime image builds at once; the first bring-up died twice
  with ENOSPC. Every counted run therefore used
  `SPACE_KIND_RUST_ARTIFACTS=prebuilt`, with the artifact image built locally
  (Docker mode) from this branch's Rust sources. Only `scratch/` changed after
  it was built. The prebuilt SHA was the checkout's HEAD.
- **Kind data.** `SPACE_KIND_DATA_ROOT` was on the daemon's larger disk,
  because the home volume was full with other work.
- **Disk.** Before every bring-up and benchmark, free space was recorded. The
  build cache, dangling images and this task's superseded Tilt image tags
  were pruned whenever space ran short and nothing was building. Stale tagged
  images were not pruned (`docker image prune -af`), because that would have
  removed the prebuilt artifact image. The Docker host never fell below
  12 GiB during a counted run.
- **The first attempt.** Run 1's first cluster never reached a suite (both
  bring-ups hit ENOSPC). It was deleted, and run 1 started on a new cluster.
- **Origin.** All values are local-dev constants or synthetic.
