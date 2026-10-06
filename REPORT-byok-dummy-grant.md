# Placeholder federated grant stalls BYOK cold activation: Kind reproduction

Status: **reproduced on Kind** (2026-10-06), no fix applied. Every commit on
this branch is REPRO ONLY tooling; no SPACE service code was changed.

## Summary

A federated client that cannot mint a grant sends the literal header
`x-space-federated-service-grant: dummy`. SPACE stores it as the sandbox's own
grant. Whenever credmgr has to reach the key broker for that sandbox (that
is, the org's intermediate key is not cached on that node), it presents
`dummy`. The broker rejects it as malformed, credmgr returns
`PermissionDenied`, and the encrypted rootfs preparation fails with
`cold_activation_encrypted_rootfs_prepare_failed`. The sandbox's own grant
stays preferred over the org's live real grants until it expires, so the
stall lasts until the placeholder's expiry unless the cache is warmed first.

Headline run (run 4, placeholder lifetime 3600 s, as the hypothesis
describes):

- cold start at 16:22:15Z;
- **250 rejected attempts over 57.9 min, median gap 14.0 s**;
- last rejection 17:19:56Z, then the placeholder expired at 17:20:08Z;
- at 17:20:09Z the next attempt presented the org's freshest live real grant
  (`org_fallback`) and the VM booted.

What the hypothesis got right and what needs correcting (details below):

| Hypothesis step | Result on Kind |
| --- | --- |
| 1. Client sends `dummy` with an expiry | Client-controlled. The expiry is the `x-space-federated-service-grant-expires-at` header, so the repro shortened it without a code change. |
| 2. API accepts any non-empty grant ≤ 64 KiB and stores it; credmgr prefers the sandbox's own live grant | **Confirmed.** Stored as a 5-byte blob. The own grant wins over fresher org grants. |
| 3. DEK call presents `dummy`; broker rejects it as malformed; credmgr gives PermissionDenied; activation fails | **Confirmed**, with the exact chain of messages (below). |
| 4. ~14 s retries until the placeholder expires (~1 h), then fallback to the org grant | **Confirmed** (run 4: 57.9 min, 14.0 s median, fallback 1 s after expiry). The loop is kubelet's `RunPodSandbox` backoff on a pod left behind by a failed start. A plain ECW create cannot stall this long: the API gives up at 120 s and deletes the sandbox. See "Which path stalls"; a same-org sibling on the same node can also end the stall early. |

## Environment

- Kind harness from `main` (84a1d9ea) on a Linux/KVM devbox, set up per
  `infra/space/kind/README.md`.
- Host `dm_crypt` and `dm_snapshot` loaded with
  `space_smoke_prepare_encrypted_targets`.
- `scripts/tests/smoke-kind-e2e.sh` passed.
- The existing BYOK Kind scenario `local.encrypted_resuspend_preserves_cow`
  passed against Kind's in-memory KEK before any repro change.
- Kind's BYOK scenarios use the in-memory KEK and never present a grant. The
  in-repo broker fixture (`crates/apps/mock/branching-key-provider`) ignores
  `AuthorizationEvidence`.
- The repro therefore adds a **broker test double**,
  `repro/byok-dummy-grant/broker_double.py`:
  - it implements the public v2 `IntermediateKeyBroker` contract with an
    evidence-required policy;
  - it rejects evidence that is not base64url-decodable as
    `AUTHORIZATION_REJECTED "space grant is malformed"`;
  - it accepts a synthetic base64url JSON grant `{sub, org, exp}`;
  - it returns `max_cache_ttl = 120 s`, like the in-repo mock.
- `repro/byok-dummy-grant/kind-broker-env.sh apply` points both worker
  credmgr authorities at the double (`SPACE_PLATFORM_KEK_PROVIDER=intermediate-key-broker`
  plus the `SPACE_CREDMGR_INTERMEDIATE_KEY_BROKER_*` group). `restore` puts
  the chart literals back.
- The trusted federated caller is a synthetic org with
  `federated_owner_assertions_enabled` keys for both the ECW and native
  surfaces, seeded with `scripts/seed-api-key.sh`.
- Federated orgs are synthetic upstream ids: `repro-upstream-org-{a,b,c,d}`.
- Raw evidence (broker decisions, per-run timelines and samples, run-4 kubelet
  events) is in `repro/byok-dummy-grant/evidence/`; run ids map as D1–D4 → run1,
  B-D → run2, C-D → run3, run 4 → run4.

Each worker's credmgr is its own authority, with its own intermediate-key
cache keyed by `(org, key id)`. An entry lives for
`min(900 s, broker cap, grant lifetime)` minus up to 10 % jitter, which is
≤ 120 s here. **Whether the placeholder is presented at all depends on that
cache.** That is why the experiments below control it explicitly.

## Evidence

### Step 2: the stored grant

The API accepted the create (201). From `sandbox_federated_service_grants`
(columns: sandbox | len | is_placeholder | expires | live | lifecycle):

```
01a111c3-6475-…  | 5  | t | 15:34:34+00 | t | stopped    <- D4, own grant = "dummy"
01a111b8-e8d4-…  | 91 | f | 15:58:13+00 | t | started    <- org's real grants, all live
01a111c3-5a6d-…  | 91 | f | 16:09:37+00 | t | started
```

The org has fresher, live real grants, yet every broker call for D4 presented
the 5-byte placeholder (`subject=repro-user-1, evidence_len=5`).

**Store ordering also protects the placeholder.** Probe S1 sent a real renewal
with an expiry earlier than the placeholder's and no `issued-at`. It was
silently not stored (the row still read `5 | t | 15:34:34`). An unstamped
renewal supersedes only with a later expiry
(`federated_service_grants.rs:129-136`). So a client that sends `dummy` with a
3600 s expiry and later a real grant with a shorter lifetime does not displace
the placeholder.

### Step 3: the DEK rejection text and code

The broker double logged, for every attempt:

`Unwrap AUTHORIZATION_REJECTED "space grant is malformed" evidence_len=5`

The kubelet event shows the whole chain with its codes (create path; the start
path is identical with `sbx-manager Start`):

```
FailedCreatePodSandBox: rpc error: code = PermissionDenied desc = failed to start sandbox "…":
  failed to create shim task: sbx-manager Wake [space-err=PermissionDenied]:
  encrypted rootfs preparation failed: node-manager PrepareEncryptedRootfs (PermissionDenied):
  credmgr GetOrIssueSandboxVolumeDek: wrap authorization was rejected: space grant is malformed
```

The sandbox-manager event `cleanup_requested` carries
`reason = cold_activation_encrypted_rootfs_prepare_failed` once per attempt
(D4 had 11 on one node within its first 2 minutes).

Two things to note:

- The broker RPC is `UnwrapIntermediateKey`. credmgr has to unwrap the org's
  intermediate key before it can wrap a new DEK or unwrap an existing one.
- credmgr does not negative-cache `AuthorizationRejected`, so every retry
  reaches the broker.

### Step 4: retry cadence, and which path stalls

| Run | Path | Cadence of rejections | How it ends |
| --- | --- | --- | --- |
| D1, D3 | ECW `POST /sandboxes` with `dummy`, landing on a cold node | 9 attempts, 13–16 s apart (kubelet `RunPodSandbox` backoff) | **API gives up at 120.4 s.** `503 "timed out waiting for sandbox pod to become ready (phase: pod_never_ready)"`; the sandbox row is deleted and the grant cascades with it. |
| D4, B-D, C-D | Create with `dummy` on a **warm** node (201 in 2.3 s, no broker call), then native `:stop` then `:start` after the cache cooled | 7–8 s while the start operation re-creates the pod (one pod generation per attempt, g2…g17) | Start operation fails after 16 attempts (~110 s, `retry_attempts_exhausted`, `DEFAULT_MAX_RETRY_ATTEMPTS = 16`), **but its last pod is left behind**. |
| (same) | The left-behind `ContainerCreating` pod | **13–16 s** (kubelet backoff; e.g. 15:13:41, 15:13:56, 15:14:09) | Retries indefinitely. Nothing deletes it: the stuck-ContainerCreating GC is off by default. Ends when an attempt succeeds (next row). |

So **the ~1 h, ~14 s stall is the orphaned pod's kubelet loop**. The
hypothesis's cadence and duration match it; the create path cannot produce
it, because the API deletes the sandbox after 120 s.

The precondition is that the placeholder sandbox **was created while its
node's cache was warm** (any recent sibling activity in the same org on that
node). In a busy org that is the common case, so the placeholder sits latent
until a later cold activation.

### Recovery

Two ways out of the orphaned-pod loop were observed:

- **Expiry fallback (D4, 1500 s placeholder; the 3600 s run is under Timings).**
  - 15:34:23Z: last attempt presenting the placeholder (`len=5`), rejected.
  - 15:34:34Z: the placeholder expires.
  - 15:34:35Z: the very next kubelet attempt gets `Unwrap OK, len=91`.
  - The presented grant's SHA-256 prefix `29911621e9c1` is the stored grant
    of sibling `01a111c3-5a6d-…`, the org's freshest live real grant
    (expires 16:09:37). That is the `org_fallback` branch of
    `live_federated_grant_prefer_sandbox`.
  - The left-behind pod went `Running`. Fallback took one kubelet retry
    after expiry, and no SPACE retry or operator action was involved.
- **Expiry fallback with a 3600 s placeholder (run 4).** The same sequence
  happened at 17:19:56 (rejected), 17:20:08 (expiry) and 17:20:09
  (`Unwrap OK`). The presented grant `2ebf0c2bc7ab` is the control sandbox
  `01a11203-e03d-…`, the org's freshest live grant (expires 19:20:06).
- **Early rescue by a sibling (C-D).**
  - About 35 s into C-D's orphan loop, a real-grant create in the same org
    landed on the same worker (15:22:26Z, `Unwrap OK`). That warmed the
    `(org, key)` cache entry.
  - On the next kubelet retry the placeholder sandbox started without any
    broker call. This was about 55 minutes before its placeholder would
    have expired.
  - In production, any same-org activation on the same node within the
    cache TTL will shorten the stall. Stalls shorter than the placeholder
    lifetime are expected in busy orgs and do not contradict the mechanism.

**What "the sandbox starts" means here.** In both recoveries the VM booted on
the node (`runtime_activated`, `guest_ready`), but the start operation had
already failed. The control plane kept the sandbox at
`lifecycle = stopped, placement = unplaced`, and the public API reported
`stopped`. The running pods were not reconciled during the observation window:

- D4 was still `Running` and `stopped` 25 min after recovery, and C-D 38 min
  after.
- They went `Completed` only when the sandboxes' ECW timeout deleted them.
- Run 4 was still `Running` and `stopped` when cleanup started.

So a report that "the sandbox started after ~1 h" may describe the node-level
start, not a successful public start.

### Controls

| Control | Result |
| --- | --- |
| C1: encrypted federated create with a **real** grant, cold cache | 201 in 2.8 s. The broker logged `Issue OK` (lazy KEK provisioning, `org_master_keys.provider = external-branching`) and `Unwrap OK` with the sandbox's own 91-byte grant. No stall. |
| R2–R5 and every warm-up: real-grant creates | All 201 in ~2 s. |
| N1: federated create with **no grant headers** | **Not allowed.** `401 "Invalid or missing API key"`; the API logs "rejected federated ECW request with invalid federated service grant: federated service grant header is missing". |
| N2: `dummy` without `-expires-at` | `401`, "federated service grant expiry header is missing". |

N1 is presumably why a client falls back to a placeholder: SPACE requires
some grant on every federated create.

## Where the placeholder should be rejected or ignored

Nothing below is implemented; the owner has the fix on hold.

1. **Stored-grant preference (the stall mechanism).**
   `crates/libs/common/svc/src/sql/federated_service_grants.rs:258`
   `live_federated_grant_prefer_sandbox`, specifically line 276:
   `ORDER BY (grants.sandbox_id = $4) DESC, grants.grant_expires_at DESC`.
   It is called from
   `crates/libs/credential/mgr/server/src/intermediate_key_identity.rs:226-249`
   (`PgIntermediateKeyIdentityLookup::identity_for_sandbox`; the selection
   is labelled `own_sandbox` / `org_fallback` at :194-195). It is reached
   through `GrpcIntermediateKeyBroker::wire_identity`
   (`intermediate_key_broker.rs:801-818`).
   - The selection has no notion of a grant the broker has already refused.
     The own grant wins for its whole lifetime.
2. **Recommended place for the fix that falls back at once: credmgr's broker
   client.**
   - Where: `GrpcIntermediateKeyBroker::unwrap_intermediate_key` /
     `issue_intermediate_key` (`intermediate_key_broker.rs:990` / `:940`).
   - What: when the broker answers `AUTHORIZATION_REJECTED` and the
     presented grant was the requesting sandbox's own (`own_sandbox`), retry
     once with the org-wide freshest live grant from another sandbox (the
     `identity(org)` / `freshest_live_federated_grant` path, excluding the
     requesting sandbox).
   - Why here: credmgr is the only SPACE component that learns the grant is
     unusable. SPACE treats grants as opaque, and only the broker can
     validate them.
   - Optionally, mark or skip the refused row so later activations do not
     re-present it.
3. **Defence in depth at ingest.**
   - Where: `crates/libs/api/src/federated_identity/ingress.rs:55-66`
     (`FederatedServiceGrant::new` accepts any 1–65 536-byte value) and
     `federated_service_grant_from_headers` (:182). They are called on ECW
     create from `routes/e2b/lifecycle.rs:341` and persisted in the create
     transaction at `controllers/sandbox/queries.rs:1233`.
   - What: rejecting or not persisting a known placeholder there would fail
     fast or leave the org fallback in effect.
   - Limit: SPACE cannot validate an opaque grant in general, so this only
     catches known placeholder shapes. Item 2 is the general fix.
4. **Upstream client.** Sending a fabricated grant is the root input. The
   fallback in item 2 makes SPACE tolerant of it, but the client should send a
   real grant or retry minting.

Related observations to file separately:

- A start operation that exhausts its retries leaves its last sandbox pod in
  `ContainerCreating` (this is what turns a 2-minute failure into an
  unbounded kubelet loop).
- When such a pod finally starts, the VM runs while the control plane still
  records the sandbox as `stopped` / `unplaced`. See "Recovery".
- Deleting the sandbox does not remove a left-behind `ContainerCreating`
  pod.
  - B-D's sandbox (and so its grant) was deleted by its ECW timeout at
    16:15. Its pod kept retrying for 63 min until cleanup deleted it.
  - The first post-deletion attempt reached the broker with no evidence
    ("space grant is required"). Later ones failed earlier, with
    `FailedPrecondition: stopped rootfs is absent`.

## Timings (UTC, 2026-10-06)

| Run | Placeholder lifetime | Event | Time | Δ |
| --- | --- | --- | --- | --- |
| D1 | 600 s | create on cold node → first rejection → 503 | 14:58:44 → 14:58:45 → 15:00:44 | 120.4 s; 9 attempts, median gap 14.4 s |
| D3 | 1200 s | same | 15:06:54 → 15:08:54 | 120.4 s; 9 attempts, median gap 14.9 s |
| D4 | 1500 s (expires 15:34:34) | warm create 201 | 15:09:42 | 2.3 s |
| | | `:stop` → cold `:start` | 15:09:54 → 15:11:44 | |
| | | start op exhausted (16 attempts, median gap 7.9 s) | 15:13:50 | ~2 min |
| | | orphan-pod loop | 15:13:56 → 15:34:23 | 87 attempts, median gap **14.6 s** (11.5–17.1) |
| | | first attempt after expiry → `Unwrap OK` (org fallback), pod Running | **15:34:35** | 1 s after expiry; 22.8 min stalled, 103 rejections |
| C-D | 3600 s | orphan loop → sibling real-grant create on same node | 15:21:45 → 15:22:26 | rescued ~55 min before expiry |
| B-D | 3600 s (expires 16:15:00) | cold `:start` → last placeholder rejection | 15:17:07 → 16:14:52 | **57.7 min, 250 rejections, median gap 14.0 s** |
| | | confounder | 16:15:00 | the fixture's ECW `timeout: 3600` deleted every org-B sandbox and grant at the same moment, so the next attempt presented no evidence ("space grant is required"); not a fallback observation |
| **Run 4** | **3600 s (expires 17:20:08)** | warm create 201 → `:stop` → cold `:start` | 16:20:16 → 16:20:16 → 16:22:15 | create 2.3 s |
| | | start op phase (pod g1…g17) | 16:22:17 → 16:24:07 (`retry_attempts_exhausted`) | 16 attempts, ~7 s apart |
| | | orphan-pod loop | 16:24:07 → 17:19:56 | 234 attempts, median gap **14.1 s** |
| | | first attempt after expiry → `Unwrap OK` (org fallback), pod Running | **17:20:09** | 1 s after expiry; **57.9 min stalled, 250 rejections, median gap 14.0 s (6.5–17.2)** |

The stall is shorter than the placeholder lifetime by the time between the
create and the cold start (here ~2 min). An activation that happens later in
the placeholder's life stalls for correspondingly less.

## Cleanup and validation after the repro

- `kind-broker-env.sh restore` was applied. The node-stack env matched the
  captured original, the API is back on `SPACE_PLATFORM_KEK_PROVIDER=in-memory`,
  and the worker credmgrs restarted with only the in-memory KEK. The broker
  double was stopped.
- All repro sandboxes were deleted, along with the synthetic orgs, their users
  and both trusted-caller API keys.
- Completed and deleting sandbox pods were force-cleared as
  `clear_completed_sandbox_pods` does.
- One cleanup needed a node-local step. Run 4's VM had booted from the
  left-behind pod, and its control-plane row was already deleted. Every kill
  then tried an encrypted suspend, which credmgr refused with `NotFound`, so the
  pod stayed `Terminating`. That task-owned sandbox was removed with
  `space-sbxmgr delete` on its Kind node. This is one more symptom of the
  orphaned-pod state.
- Afterwards `local.encrypted_resuspend_preserves_cow` passed again on the
  restored stack.

## Reproduce

```bash
# Kind up + baseline, host dm targets (see infra/space/kind/README.md)
source scripts/lib/space-smoke-prereqs.sh && space_smoke_prepare_encrypted_targets
uv run --script repro/byok-dummy-grant/broker_double.py --listen 0.0.0.0:50051 &   # host
bash repro/byok-dummy-grant/kind-broker-env.sh apply        # http://<kind gateway>:50051
# seed an ECW and a native key with --federated-owner-assertions (scripts/seed-api-key.sh)
source repro/byok-dummy-grant/lib.sh   # bash; set CALLER_KEY_FILE, ADMIN_TOKEN_FILE, OUT
fed_create <upstream-org> <subject> real $(( $(date +%s)+3600 ))  # materialize the org
admin PATCH orgs/<space-org> '{"byok_enabled":true}'
# One-shot unattended version of the steps below (run 4 used this):
#   SANDBOX_TIMEOUT=10800 repro/byok-dummy-grant/run_latent_stall.sh "$OUT" <upstream-org> <subject> 3600
# Keep SANDBOX_TIMEOUT above the stall; otherwise the ECW timeout deletes the sandbox (see run2).
warm_then_create <upstream-org> <subject> dummy $(( $(date +%s)+600 ))
native POST /v1/sandboxes/sbx_<id>:stop  <upstream-org> <subject> '{}'
# wait > 125 s after the org's last broker Unwrap OK, then:
native POST /v1/sandboxes/sbx_<id>:start <upstream-org> <subject> '{}'
repro/byok-dummy-grant/record_stall.sh "$OUT" <upstream-org> <subject> <id> &
bash repro/byok-dummy-grant/kind-broker-env.sh restore      # afterwards
```

Limitations:

- The broker is a test double that emulates the rejection text and code from
  the hypothesis. The production broker's behaviour (including how it treats
  absent evidence) was not exercised.
- The cache TTL comes from the double's `max_cache_ttl` (120 s). A
  production broker cap changes how long a sibling's activity keeps a node
  warm, not the mechanism.
