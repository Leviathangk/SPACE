# Typed reclaim classification for pooled-create failures

Design report. Read-only analysis of commit `0b1f740` plus the characterization
tests in open PR #273 (`ac/char-tests-api-k8s-adapter`). No code changed. All
statements come from reading the code unless marked otherwise. Nothing here was
run against a cluster.

## Decision summary

- **Recommendation: replace the substring predicate with a typed
  classification.** Use the shape in §4: a `PooledPrepareFailureKind` set by
  the adapter where it maps each node status, carried as an optional field on
  `ControllerError`, and read by one action function in
  `controllers/sandbox/queries.rs`.
- **Why.** Today the reclaim decision depends on three things that do not
  describe the failure: the HTTP status picked at each `map_err`, the wording
  of our own messages, and tonic 0.14's `Display` for `Status`. The fragment
  `"service is currently unavailable"` is tonic's text for `Code::Unavailable`.
  The fragment `"deadline exceeded"` matches only the retrying client's own
  deadline message, not a server's `DeadlineExceeded`. The result: one
  condition (a persistently Unavailable node) gets three different
  classifications depending on the step and on timing (rows S10, W2 and T8 in
  §2).
- **Migration.** Five small PRs (§5). PR 1 and PR 2 preserve behavior. PRs 3a
  to 3e each make one listed behavior change. PR 4 deletes the fragment list.
  PR 5 is optional and needs an owner decision.
- **Behavior changes proposed** (§4.4):
  - (a) A network-policy transport or deadline failure becomes a reclaimable
    503 (Finding 1).
  - (b) Terminal node statuses on the route-adoption Wake and the
    auto-lifecycle step become 409 or 500, not 503 (Finding 5).
  - (c) Slot-local refusals re-claim another slot instead of falling back to
    cold: owner mismatch, a Wake acknowledgement for a stale binding, and a
    sandbox missing on the node.
  - (d) Nothing retries or re-claims once the 135 s readiness budget is spent.
    Today some started-path messages do both after the full budget.
  - (e) The unreachable `Unavailable` and `Cancelled → 408` arms are removed
    (Finding 6).
- **Deliberately unchanged:**
  - The three no-fragment 503s from Finding 4 still do not re-claim. They are
    node-wide (a stale sbx-mgr template cache, a worker contract or version
    mismatch), and re-claim cannot avoid the node yet. They now get that
    outcome from their type rather than from missing text.
  - Fallthrough creates still fall back to cold on request-fatal errors.
  - `boot_mode=pool` still never re-claims.
- **Owner decisions needed** (§8):
  - whether an in-place retry is worth anything after the retrying client has
    already spent its 5 s on the same RPC;
  - whether `boot_mode=pool` may re-claim;
  - whether the slot claim should exclude a node.

## 1. How a pooled-prepare failure travels today

1. `prepare_pooled_execution` (`controllers/substrate/kubernetes/adapter/substrate_adapter.rs`)
   and `apply_adopted_per_sandbox_setup` (`…/adapter/pooled.rs`) call node-api
   through `RetryingWorkerApiClient` / `RetryingWorkerApiInternalClient`
   (`pplx_space_node_api_client::{WorkerApiClient, WorkerApiInternalClient}`).
   Each call runs `GrpcClient::call` (`common/grpc/src/client.rs`) with
   `DEFAULT_TCP_GRPC_CONNECT_RETRY_POLICY`: up to 12 retries, 10 s
   `max_elapsed`.
2. That client retries `Unavailable`, `Unknown`, and `Internal`/`Cancelled`
   whose text matches `TEMPORARY_TCP_FAILURE_FRAGMENTS`
   (`retry_delay_for_status` in `common/grpc/src/retry.rs`). It never retries
   a status marked `refused_before_start`. Retries stop on one of two budgets:
   - The caller's context. Setup steps use `WARM_POOL_SETUP_RPC_TIMEOUT` = 5 s.
     When it runs out, the client returns
     `DeadlineExceeded("worker-node-api/<cmd> deadline exceeded while waiting to retry|running RPC|starting attempt")`,
     and the last real status is dropped (`deadline_exceeded_status`,
     `retry_delay_fits_budget`).
   - The policy's own 10 s `max_elapsed`. Only calls given more than 10 s reach
     it, which here means only the started-path Wake. The client then returns
     the last real status unchanged.
3. Each step maps the `tonic::Status` to a `ControllerError` with
   `format!("…: {err}")`. In tonic 0.14.6, `Display for Status` renders
   `code: '<code description>', message: "<message>"`.
4. The callers in `controllers/sandbox/queries.rs` decide with
   `pooled_prepare_error_is_reclaimable`: `is_lock_contention()`, or status
   503/408 **and** the lowercased message contains one of
   `POOLED_PREPARE_RECLAIMABLE_ERROR_FRAGMENTS`.
5. On Kubernetes every failed attempt consumes its slot, whatever the
   decision. `cleanup_failed_pooled_prepare` marks the slot `cleanup_pending`
   and returns `DeferredToReconciler`. Re-claim takes the oldest ready slot
   (`WarmPoolStore::claim_ready_slot*`, `ORDER BY ready_at`, `SKIP LOCKED`),
   with no regard to node.

Node-side codes below come from tracing node-api (`node/api/server`) into
sandbox-manager (`sbx/mgr/server/src/service/…`). On these four RPCs the only
`refused_before_start` status is node-api's per-sandbox limiter
(`ResourceExhausted`, `uds_per_sandbox_limit_exceeded_status`). Node-api
drain (`Unavailable "node-api draining"`) and sbx-mgr restart drain are plain
`Unavailable`, so the client retries them.

## 2. Inventory

"Reclaim today" means `pooled_prepare_error_is_reclaimable` is true, which
allows both the in-place retry and the re-claim. Nature: **Slot** means another
slot would likely succeed. **Node** means every slot on this node would fail
the same way. **Req** means a request or control-plane problem that no slot
would fix. **Infra** means config or transport shared by every node.
**Budget** means the readiness budget is spent. **Defect** means a broken
invariant.

### 2.1 Caller-side and preamble

| ID | Where | Source → after retrying client | HTTP | Message (prefix) | Reclaim today | Nature |
|---|---|---|---|---|---|---|
| P1 | `prepare_pooled_execution_with_retries`, policy reload | `55P03` lock race → `lock_contention()` | 503 | "operation conflicted with a concurrent change; retry the request" | yes (kind) | Req, transient |
| P2 | same | other DB error → `db_contention_error` | 500 | "failed to reload pooled lifecycle policy: …" | no | Infra |
| P3 | same | row missing | 404 | "sandbox" | no | Req |
| P4 | `prepare_pooled_execution`: `PodIp::parse`, `endpoint_for_pod_ip` | `SubstrateError::Failed` | 500 | "invalid pod IP …" / endpoint error | no | Defect (the claim reads `host(pod_ip)` from an `inet` column, so this is practically unreachable) |

### 2.2 Non-started capability check (`wait_until_ready = false`)

| ID | Where | Source → after client | HTTP | Message | Reclaim | Nature |
|---|---|---|---|---|---|---|
| N1 | `health_service_serving` (route-identity capability) | transport / draining `Unavailable` → retried → client `DeadlineExceeded` | 503 | "pooled sandbox route-identity capability check failed: … deadline exceeded while …" | yes | Node / transport |
| N2 | same | `Unauthenticated` / `PermissionDenied` (not retried) | 503 | same prefix | no | Infra |

`NotFound` and `Unimplemented` mean "older worker". They become `Ok(false)`
(`health_capability_unsupported`) and are not errors.

### 2.3 Per-sandbox setup (`apply_adopted_per_sandbox_setup`, both modes)

| ID | Step | Source → after client | HTTP | Message | Reclaim | Nature |
|---|---|---|---|---|---|---|
| S1 | `update_sandbox` | `FailedPrecondition "sandbox … owner is not <root>"` (compare-and-set lost: already adopted) | 409 | "pooled sandbox metadata update failed: …" | no | **Slot** |
| S2 | `update_sandbox` | `FailedPrecondition "cannot update: sandbox is X, not started or paused"` | 409 | same | no | **Slot** |
| S3 | `update_sandbox` | transport / draining / restart-drain `Unavailable` → retried → client `DeadlineExceeded` | 503 (`pooled_metadata_update_error_is_reclaimable`) | same + "deadline exceeded while …" | yes | transport / Node |
| S4 | `update_sandbox` | server `DeadlineExceeded` (not retried). For example node-api's "restart handoff deadline exceeded" | 503 | same | yes only if the server's text happens to contain "deadline exceeded" | transport / Node |
| S5 | `update_sandbox` | `Cancelled` with no TCP text (not retried) | 408 | same | no (no fragment) | effectively unreachable (Finding 6) |
| S6 | `update_sandbox` | `NotFound` (sandbox missing on node), `Internal` (node DB) | 500 | same | no | **Slot** |
| S7 | `update_sandbox` | `InvalidArgument`, `PermissionDenied` (owner label not allowed for peer) | 500 | same | no | Req / Defect |
| S8 | any setup RPC | `ResourceExhausted` + `refused_before_start` (node-api limiter; not retried) | 500 (update, policy), 503 no fragment (auto-lifecycle, Wake) | step prefix | no | Node, transient with a retry hint |
| S9 | `set_sandbox_network_policy` | `FailedPrecondition` (generation conflict, "not valid for a pool member", policy cache superseded) | 409 | "pooled sandbox network policy update conflicted: …" | no | Slot / Node |
| S10 | `set_sandbox_network_policy` | `Unavailable` ("requires a live net-gateway", "refresh failed", "refresh timed out"), transport, draining → retried → client `DeadlineExceeded` | **500** | "pooled sandbox network policy update failed: …" | **no** (Finding 1) | **Slot** (per-sandbox gateway) or Node |
| S11 | `set_sandbox_network_policy` | `InvalidArgument`, `PermissionDenied`, `Internal` | 500 | same | no | Req / Defect / Node |
| S12 | `set_sandbox_network_policy` response | undecodable `effective_policy` | 500 | "pooled sandbox returned an undecodable effective network policy" | no | Defect (version skew) |
| S13 | `set_sandbox_network_policy` response | confirmed policy ≠ resolved policy | 503 | "…gateway confirmed a network policy that differs from the request-resolved policy…" | no (Finding 4) | **Node** (sbx-mgr template cache) |
| S14 | `set_sandbox_auto_lifecycle_policy` | transport / draining → retried → client `DeadlineExceeded` | 503 | "pooled sandbox data-plane auto lifecycle policy update failed: …" | yes | transport / Node |
| S15 | `set_sandbox_auto_lifecycle_policy` | `InvalidArgument`, `FailedPrecondition` (state), `PermissionDenied`, `Internal` | **503** | same | no, unless the server text contains a fragment (Finding 5) | Req / Slot / Defect |

The network-policy override annotation patch runs on a detached
`tokio::spawn` and cannot fail the adoption.

### 2.4 Non-started route-identity adoption

| ID | Where | Source → after client | HTTP | Message | Reclaim | Nature |
|---|---|---|---|---|---|---|
| W1 | `pooled_route_identity_adoption_request` / `pooled_route_activation_id` | claim lacks pod UID or pool slot id | 500 | "Kubernetes pooled claim is missing its …" | no | Slot (legacy row) / Defect |
| W2 | `wake_sandbox` | transport / draining / `Unavailable "activation admission is busy; retry"` → retried → client `DeadlineExceeded` | 503 | "pooled sandbox route identity adoption failed: …" | yes | transport / Node |
| W3 | `wake_sandbox` | `Aborted` (route epoch or operation conflict), `FailedPrecondition`, `NotFound`, `InvalidArgument`, `PermissionDenied`, server `DeadlineExceeded "sandbox did not become active in time"` | **503** | same | no (Finding 5) | Slot (Aborted / NotFound / FailedPrecondition) or Req / Defect |
| W4 | `validate_wake_route_identity_with_node_requirement` | a worker that advertises receipts omits `current_node_id` | 503 | "sandbox-manager Wake acknowledged a different current_node_id route identity" | no (Finding 4) | **Node** (worker contract) |
| W5 | same | any other dimension differs (`pod_uid`, generations, attempt, `sandbox_id`), or the identity is missing | 503 | "…acknowledged a different <dim> route identity" / "…omitted the authoritative route identity" | no | **Slot** (stale binding) |

### 2.5 Started adoption (`wait_until_ready = true`)

| ID | Where | Source → after client | HTTP | Message | Reclaim | Nature |
|---|---|---|---|---|---|---|
| T1 | `wait_for_pooled_split_receipt_capability` before setup (5 s) | NOT_SERVING or unsupported | 503 | "pooled sandbox operation-receipt capability was not ready before split activation: worker node-api reported operation receipts as NOT_SERVING" | no (Finding 4) | **Node** (worker version) |
| T2 | same | the health check itself fails | 503 | same prefix + last error text | yes (the text carries "deadline exceeded" or tonic's Unavailable description) | transport / Node |
| T3 | `assign_activation_attempt`, `reconcile_activation_fence`, `publish_legacy_route_identity_after_ack` | `SubstrateError::LockContention` | 503 (kind) | contention message | yes | Req, transient |
| T4 | same | `ActivationInFlight` for another target | 503 + retry hint (the caller strips it) | "…" | no | Slot (another activation owns it) |
| T5 | same | `Failed` / other | 500 etc. | "…" | no | Infra / Defect |
| T6 | `activation_wake_action` → `k8s_execution_route_identity` | "no published route identity; retry after materialization" / invalid generation | 503 / 409 | "Kubernetes execution has …" | no | Slot row / Defect |
| T7 | split-receipt **recheck** before the Wake (budget: the remaining ≈135 s) | as T1 / T2, but with the readiness budget | 503 | as T1 / T2 | T2 form: **yes, after up to the full budget** | Node / Budget |
| T8 | Wake loop: `Unavailable` (the client returns the raw status after its 10 s `max_elapsed`) and not-ready `FailedPrecondition`, polled until the budget runs out | | 503 | "pooled sandbox Wake did not complete within the readiness budget[: {error}]" | **yes** when `{error}` renders an Unavailable status; no for the bare form | Budget |
| T9 | Wake: `classify_wake_rpc_error` → `Fail` (`InvalidArgument`, `Aborted`, `NotFound`, other `FailedPrecondition`, `ResourceExhausted`, and the client `DeadlineExceeded` when the per-call deadline equals the remaining budget) | | 409 | "sandbox wake failed: …" | no | Slot (refusal) or Budget (deadline) |
| T10 | Wake OK, but `validate_wake_route_identity_if_present` fails | | 503 | as W4 / W5 | no | as W4 / W5 |
| T11 | `poll_connected_client_until_command_ready` | a probe error other than not-started or `Unavailable` | 500 | "sandbox failed before start completed: …" | no | Slot or Req (ambiguous) |
| T12 | same | readiness deadline | 503 + retry hint (stripped) | "timed out waiting for sandbox command readiness (phase: …)" | no | Budget |

**Rows T7 and T8 are nondeterministic.** A started Wake against a node that
stays Unavailable can end in any of three ways, depending on which deadline
check fires first: a 409 from T9, a 503 that carries a fragment from T8, or a
bare 503 from T8. Only the second retries in place and re-claims, and it does
so after the 135 s budget is gone.

**Direct substrate.** `DirectSubstrate::prepare_pooled_execution` maps every
resume error to 500 ("node-api resume error"), so it never reclaims. Its
`worker_client_for_sandbox` errors carry "node-api not reachable", which is
the only source of that fragment on this path. Any typed design must tag
Direct too before the fragments can go.

## 3. The callers and what the client sees

**Who calls the predicate.** All three callers are in
`controllers/sandbox/queries.rs`.

| Caller | Uses the predicate for | Effect |
|---|---|---|
| `prepare_pooled_execution_with_retries` | in-place retry, for every boot mode | Retry the same claim and pod. At most `MAX_POOLED_PREPARE_IN_PLACE_ATTEMPTS` = 2 attempts in total, 50 ms apart; the lifecycle policy is re-read each time. A retry is safe after a partial setup: `set_sandbox_owner_if_current` accepts an owner it already applied. |
| `try_create_from_pool_with_substrate` → `pooled_prepare_failure_action` | re-claim | `FallbackToOnDemand` (only `CreateBootMode::Fallthrough`, the default when the request has no `boot_mode`) re-claims while `prepare_attempt < MAX_POOLED_PREPARE_RECLAIM_ATTEMPTS` (3): at most 3 slots and 6 prepare calls per request. `Propagate` (`boot_mode=pool`) never reaches the predicate. `Cold` and `BaseSnapshot` never claim. |
| `try_create_from_admitted_pool_claim_with_substrate` | in-place retry only | Must adopt its admitted slot, because the public id is the slot's id. Any failure with a successful rollback becomes `admitted_pool_slot_release_pending()`: a 503 with `PoolSlotRelease`, which the async dispatcher re-queues, and the retry builds cold under the same id. |

`pooled_prepare_failure_reason` turns the same predicate into the `reason`
log and metric label (`transient_*` / `non_retryable_*`).

**What a client sees today.** Every failed attempt rolls back first, through
`rollback_failed_pooled_create_with_substrate`.

| Disposition | Predicate true | Predicate false |
|---|---|---|
| Fallthrough (public API without `boot_mode`, and all E2B/ECW creates: `translate.rs` sets `boot_mode: None`) | Re-claim. After 3 slots, the create goes cold (`ColdFallback`); the client sees the cold create's result. | Cold at once; the client sees the cold result. Under Fallthrough the classification shows only as latency and pool consumption. |
| Propagate (`boot_mode=pool`, public API only) | One in-place retry, then propagate | Propagate |
| Either, if the rollback itself fails | `pooled_create_terminal_error(error, false)` | same |

When an error is propagated on Kubernetes, the cleanup result is
`DeferredToReconciler`, so the error is always demoted
(`demoted_after_side_effect`). The demotion only removes retry hints and the
`LockContention` kind; the status is kept. The public translator
(`routes/public/sandboxes.rs::controller_error`) then maps:

- 503 → `capacity_unavailable`, which reads as retryable;
- 409 → `conflict`;
- 500 and 408 → `internal`.

The S15 and W3 rows (terminal node errors that the adapter maps to 503)
therefore tell a `boot_mode=pool` client to retry. Each retry consumes another
slot, while the first sandbox row stays `paused` until the reconciler collects
it. ECW/E2B never sees a propagated prepare error, except after a failed
rollback.

## 4. Proposed typed design

### 4.1 Shape

```rust
// controllers/substrate/mod.rs (next to PooledPrepareFailureCleanup)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PooledPrepareFailureKind {
    /// Retry the same claim: nothing about the slot was wrong (lock race,
    /// one-shot transport loss).
    Transient,
    /// This slot cannot be adopted; another slot may succeed.
    SlotLocal,
    /// Every slot on this node would fail the same way (worker contract or
    /// version, node-wide cache, node saturation).
    NodeLocal,
    /// No slot would help: the request, control-plane state, configuration,
    /// or a broken invariant.
    RequestFatal,
    /// The readiness budget is spent; retrying would restart it.
    BudgetExhausted,
}
```

The kind is carried as `pooled_prepare: Option<PooledPrepareFailureKind>`, a
new private field on `ControllerError`, with `with_pooled_prepare_kind(kind)`
to set it and `pooled_prepare_kind()` to read it. A separate field, not a new
`ControllerErrorKind` variant, for three reasons:

- An error can be `LockContention` and `Transient` at once.
- The surface translators key off `ControllerErrorKind` and must not change.
- `without_retry_after()` and `demoted_after_side_effect()` leave the field
  alone, so the classification survives to the decision point.

The kind is set where the status is mapped. One table function in
`adapter/pooled.rs` replaces the five ad-hoc `map_err` closures:

```rust
fn pooled_setup_failure(step: PooledSetupStep, status: &tonic::Status)
    -> ControllerError // status code + message + kind, one table
```

This function applies a single rule for every step: a client
`DeadlineExceeded` (or `Unavailable`) is `Transient`; `refused_before_start`
is `NodeLocal`; `FailedPrecondition`, `Aborted` and `NotFound` are
`SlotLocal`; `InvalidArgument`, `PermissionDenied` and `Unauthenticated` are
`RequestFatal`. Each step may override the rule where its row needs it (S13,
S15, W4, T1). The non-RPC refusals (S12, S13, W4, W5, T1) and the started-path
budget exits (T7 and T8 after the deadline, T9 when the deadline expires,
T12) are tagged where they are built. In the started path, an expired
deadline is detected by checking `wake_deadline`, never by reading message
text.

On the caller side, two functions in `queries.rs` take over:

```rust
fn pooled_prepare_failure_kind(error: &ControllerError) -> PooledPrepareFailureKind {
    if error.is_lock_contention() { return Transient; }
    error.pooled_prepare_kind().unwrap_or_else(|| legacy_kind(error)) // PR 4 removes legacy_kind
}
fn pooled_prepare_retry_in_place(kind) -> bool { kind == Transient }
fn pooled_prepare_failure_action(boot_mode, kind, attempt) -> PooledPrepareFailureAction
```

`legacy_kind` is today's predicate, mapped to `Transient` when true and
`RequestFatal` when false, so untagged errors (Direct, test doubles) behave
exactly as they do now until PR 4.

### 4.2 Action matrix

| Kind | In-place retry | Re-claim (Fallthrough, attempt < 3) | Then | `boot_mode=pool` status |
|---|---|---|---|---|
| Transient | yes | yes (today's behavior) | cold | 503 |
| SlotLocal | no | **yes** | cold | 503 |
| NodeLocal | no | no, until the claim can exclude a node (PR 5) | cold | 503 |
| RequestFatal | no | no | cold (unchanged) | 409 / 500 / 400 as mapped |
| BudgetExhausted | no | no | cold | 503 |

`pooled_prepare_failure_reason` becomes the kind's snake_case name. That
changes the metric label values; dashboards and monitors that read `reason`
must be updated (see `space-observability`).

### 4.3 Classification of every row

| Kind | Rows |
|---|---|
| Transient | P1, N1, S3, S4 (every server `DeadlineExceeded`, not only those whose text matches), S10, S14, W2, T2 (pre-setup only), T3 |
| SlotLocal | S1, S2, S6, S9 generation conflict, W1 (a slot without a pod UID), W3 `Aborted` / `NotFound` / `FailedPrecondition`, W5, T4, T6, T9 refusals, T10 as W5 |
| NodeLocal | S8, S9 "not valid for a pool member", S13, W4, T1, T7 NOT_SERVING, T10 as W4 |
| RequestFatal | P2, P3, P4, N2, S7, S11, S12, S15 (all non-transport codes; the `FailedPrecondition` state case could be SlotLocal, see §8), W3 `InvalidArgument` / `PermissionDenied`, T5, T11 (ambiguous, kept non-reclaimable) |
| BudgetExhausted | T7 and T8 once `wake_deadline` has passed, T9 client deadline at budget, T12 |
| removed | S5 and the `Code::Unavailable` arm of S10: both collapse into the table function |

### 4.4 Rows whose behavior changes

| Change | Rows | Today → proposed | Why this is right |
|---|---|---|---|
| (a) | S10 | 500, no reclaim → 503 Transient | The gateway is a per-sandbox sidecar, and the code comment says a dead gateway should roll back "this claimed pool pod" (it already does). The metadata step already treats the same client deadline as reclaimable. The PR's reproduction case becomes the test. |
| (b) | S15, W3 non-transport codes | 503 → 409 (`FailedPrecondition`, `Aborted`) or 500 (others) | These are the same node answers the other steps already map to 409 or 500. A 503 tells a `boot_mode=pool` client to retry, and each retry burns a slot. Under Fallthrough nothing changes except the label and, for SlotLocal codes, (c). |
| (c) | S1, S2, S6, W3 SlotLocal codes, W5, T4, T9 refusals | cold fallback → re-claim (Fallthrough only) | The failure belongs to this pod: it was already adopted, its sbx-mgr lost the sandbox, or the Wake answered for a different binding. The slot is consumed either way, so re-claiming costs at most two more slots and saves a cold boot. `pooled_create_fallback_tests::pooled_prepare_non_transport_failure_does_not_reclaim` changes deliberately for the owner-conflict input. |
| (d) | T7, T8 (fragment-bearing forms) | in-place retry + re-claim after up to 135 s → stop and go cold | A retry restarts a full readiness budget. At up to two in-place attempts × three slots, that can multiply create latency for an outcome the client is already waiting on. |
| (e) | S5, the `Unavailable` arm of S10 | arms removed | Unreachable through the retrying client (Findings 1 and 6). If a server-side plain `Cancelled` ever arrived, it would map to RequestFatal / 500, the same disposition as today's 408 (no reclaim). |
| side effect | S4, S15, W3 | A text accident no longer decides the outcome. A server `DeadlineExceeded` without the "deadline exceeded" text now retries; a terminal code whose text contains "connection refused" no longer re-claims. | Removes the false positives and false negatives of substring matching. |

Not changed, on purpose: S13, W4, T1 and T7 NOT_SERVING (the Finding 4 trio).
Re-claim cannot avoid the node, and these failures are node-wide, so a
re-claim would most often land on the same node or another old one. The
existing unit test that pins S13 as non-reclaimable stays valid.

## 5. Migration

Each PR is small and lands green on its own.

1. **PR 1: type and plumbing (behavior-preserving).**
   - Add `PooledPrepareFailureKind`, the `ControllerError` field and its
     accessors, and the caller-side `pooled_prepare_failure_kind`,
     `pooled_prepare_retry_in_place` and the reworked
     `pooled_prepare_failure_action`, with `legacy_kind` as the fallback.
   - Document the contract on `SubstrateAdapter::prepare_pooled_execution`.
   - Every existing test passes unchanged, including `pooled_create_fallback_tests.rs` and
     `create_postgres_tests.rs::postgres_pooled_create_retries_prepare_in_place_before_reclaiming`.
2. **PR 2: tag the Kubernetes adapter (behavior-preserving except the listed
   side effect).**
   - Add `pooled_setup_failure` and tag every row with a kind that reproduces
     today's reclaimable bit: Transient wherever today's predicate is true,
     RequestFatal elsewhere.
   - Status codes are unchanged, so every #273 case passes as is.
   - Add a kind assertion beside each existing status assertion in
     `pooled_adoption_stops_at_a_failed_setup_step` and
     `pooled_route_adoption_requires_a_complete_acknowledgement`.
   - The only behavior change is the substring side effect in §4.4. List it
     in the PR.
3. **PRs 3a to 3e: one behavior change each**, in the order (e), (a), (b),
   (d), (c). Do the cheap ones first. Do (c) last, because it is the only one
   that increases pool consumption. Tests that change deliberately:
   - (e): #273 `pooled_adoption_stops_at_a_failed_setup_step::update_cancelled`
     (408). The PR's own follow-up list already suggests dropping it.
   - (a): add the Finding 1 case, `SetupFailure::NetworkPolicy(Status::unavailable(…))`,
     expecting 503 Transient. It runs on real time for about 5 s; consider a
     smaller setup budget for tests.
   - (b): new cases for an auto-lifecycle `FailedPrecondition` and a
     route-adoption Wake `Aborted`. #273 left both unpinned (Finding 5).
   - (d): new started-path cases. They need the injectable readiness budget
     the #273 follow-ups already ask for, because `NODE_RPC_BLOCKING_BUDGET`
     is a compile-time constant.
   - (c): `pooled_prepare_non_transport_failure_does_not_reclaim`
     (owner-conflict half). Also
     `pooled_route_adoption_requires_a_complete_acknowledgement::capable_worker_must_report_its_node`:
     it stays a failure but gains a NodeLocal assertion, and a new W5 case
     asserts SlotLocal.
4. **PR 4: delete the fragments.**
   - Tag Direct's `prepare_pooled_execution`: "node-api not reachable" is
     Transient, the resume error stays RequestFatal.
   - Retag the test doubles: `create_postgres_tests.rs` doubles return the
     fragment-bearing "status: Unknown, message: \"transport error\"", which
     is a rendering tonic 0.14.6's `Display` does not produce.
   - Remove `legacy_kind` and `POOLED_PREPARE_RECLAIMABLE_ERROR_FRAGMENTS`.
     Untagged errors become RequestFatal, which is today's answer for any
     error without a fragment.
   - Rewrite `pooled_prepare_transport_failure_reclaims_before_cold_fallback`
     and `pooled_prepare_cancelled_deadline_does_not_reclaim` over kinds.
   - `adapter::tests::pooled_metadata_update_deadline_is_reclaimable` moves
     to the table function's tests.
5. **PR 5 (optional, owner decision):**
   - A claim option that skips the failed attempt's node, so NodeLocal can
     re-claim.
   - Possibly re-claim under `boot_mode=pool`.
   - Possibly return an untouched slot to `ready` for refusals that happen
     before any setup (T1, N2), instead of marking it `cleanup_pending`.

#273 tests that pin today's behavior and stay valid without edits: every
status assertion except `update_cancelled`; `started_pooled_adoption_releases_its_attempt_only_when_no_wake_may_be_pending`
(its 409 for a terminal started Wake is kept); `started_pooled_adoption_refuses_a_worker_without_split_receipts`
(it gains a NodeLocal assertion in PR 2). The three `"deadline exceeded"`
fragment assertions (`update_deadline`, `auto_lifecycle_deadline`,
`wake_deadline_is_reclaimable`) become kind assertions in PR 4. They pin the
caller's dependency on the text, and that dependency is what PR 4 removes.

## 6. Risks

**Reclaim storms.** On Kubernetes a failed attempt always consumes its slot.
A re-claim therefore costs at most `MAX_POOLED_PREPARE_RECLAIM_ATTEMPTS - 1`
= 2 extra slots per Fallthrough create. The danger is a request-fatal error
misclassified as SlotLocal. Every create would then burn 3 slots instead of 1,
so N concurrent creates drain 3N slots per wave until the pool misses and
creates go cold. Refill is bounded by the producer, so warm hits stay low
until the misclassification is fixed. Mitigations:

- Keep ambiguous rows non-reclaimable (S15 `FailedPrecondition`, T11).
- Introduce (c) last and on its own.
- Watch `warm_pool::acquire_duration{outcome="miss"}` and the per-kind
  `reason` label on the "warm pool claim failed, retrying" log.
- Optionally cap re-claims per template over a short window: a circuit
  breaker that downgrades SlotLocal to RequestFatal when the re-claim rate
  passes a threshold. It needs shared state, so it is out of scope for PRs
  1 to 4.

The NodeLocal rows are the most storm-prone. During a rolling upgrade many
nodes can be old (T1, W4). Oldest-ready ordering would keep landing re-claims
on them, which is why they stay non-reclaimable until PR 5.

**Retry amplification.** Each prepare already contains bounded retries
(5 s per setup RPC inside the client). The worst cases:

- Non-started Transient today: 2 in-place attempts × 3 slots × about 25 s
  (five 5 s RPC budgets) before the cold fallback.
- Change (a) adds S10 to that set. With a dead gateway the create now pays
  5 s, then 5 s more in place, then up to 2 more slots, instead of going cold
  after 5 s. That is up to about 30 s on the network-policy step alone.
- Started path today (T7/T8): up to 2 × 135 s per slot. Change (d) removes
  this.

The in-place retry adds little for transport errors, because the client has
already retried the same RPC for its whole 5 s budget. Making client-deadline
errors SlotLocal instead of Transient would drop the in-place retry for them
and keep it for lock contention. That cuts the worst case by half, but it
changes `postgres_pooled_create_retries_prepare_in_place_before_reclaiming`,
so it is left to the owner (§8).

**Classification drift.** A new `map_err` in adoption code that forgets the
kind falls back to RequestFatal after PR 4. That is safe (no reclaim), but it
fails silently. A unit test over `pooled_setup_failure`'s table, plus the #273
per-step cases asserting kinds, makes drift visible.

**Mixed versions.** All classification happens API-side from codes the node
already returns. No proto or node change is required.

## 7. Alternatives

| Option | What it fixes | Costs |
|---|---|---|
| **Typed kind on `ControllerError` (recommended)** | All findings. Classification sits next to the status. | One field. One table function. About 8 tests updated over 5 PRs. |
| Dedicated error type at the trait boundary (`Result<(), PooledPrepareError>`) | Same, and the compiler forces every adoption error to be classified | Touches `SubstrateAdapter`, the dispatch enum, Direct, Kubernetes and three test doubles. A blanket `From<ControllerError>` would undo the enforcement. A reasonable end state after PR 4, not a first step. |
| New `ControllerErrorKind` variants | — | Conflicts with `LockContention` and with the translators, which read that kind. Rejected. |
| **Just add the missing fragments** (map S10's deadline to 503; add text for S13, W4, T1) | Finding 1, and Finding 4 if wanted. Small diff. | Still couples retries to message text and to tonic's `Display`; a tonic upgrade or a reworded message silently changes reclaim behavior. Finding 5's 503s and the T7/T8 after-budget retries remain. Adding S13, W4 or T1 would be wrong anyway, because they are node-wide. |
| Make the retrying client keep the last real status on deadline (a typed source on the returned `DeadlineExceeded`) | Lets the adapter tell "gateway not live" from transport loss in S10 and N1 | Shared-crate change (`common/grpc`). Complements the typed kind; does not replace it. |
| **Do nothing; document** | — | Finding 1 means cold fallbacks on gateway flakes: slower but correct. Finding 5 means `boot_mode=pool` clients get a retryable 503 for terminal errors, and every retry burns a slot. The after-budget retry and re-claim in T7/T8 stays. Safe in the sense that substring matching mostly errs toward not re-claiming. |

## 8. Open questions and coverage gaps

- **In-place retry after a client deadline.** Keep it (today's behavior) or
  drop it, as discussed in §6?
- **`boot_mode=pool` and re-claim.** Should a strict pool create re-claim on
  SlotLocal or Transient? Today it propagates after one in-place retry.
- **Node exclusion on re-claim (PR 5)**, and whether an untouched slot
  refused before setup should go back to `ready` rather than
  `cleanup_pending`.
- **The two ambiguous rows.** S15 `FailedPrecondition "cannot update auto
  lifecycle while sandbox is X"` could be SlotLocal (the pod's VM is in a bad
  state). T11 could be either. Both stay non-reclaimable unless the owner has
  evidence.
- **Cold fallback on S13 may not avoid the stale cache.** An inheriting cold
  create also resolves the template policy through the node's sbx-mgr. Cold
  avoids the mismatch error only because nothing verifies the policy there.
  Not verified; worth checking before S13 is made reclaimable for any node.
- **What was not run.** No test was run for this report.
  - The tonic `Display` format and the retrying-client conversions were read
    from source (tonic 0.14.6 `Status`/`Code` `Display`;
    `common/grpc/src/{client,retry,deadline}.rs`).
  - T7/T8's three possible outcomes were derived by reading the code and
    were not exercised. Pinning them needs the injectable readiness budget
    from #273's follow-ups.

## Decision summary (repeated)

- **Recommendation: replace substring matching with a typed classification.**
  Use `PooledPrepareFailureKind { Transient, SlotLocal, NodeLocal,
  RequestFatal, BudgetExhausted }`, set where the adapter maps node statuses,
  carried as an optional `ControllerError` field, and consumed by one action
  function.
- **Migration.** Five PRs: PRs 1 and 2 preserve behavior, PRs 3a to 3e each
  make one listed change, PR 4 deletes the fragment list, PR 5 is optional.
- **Changes:**
  - (a) network-policy transport and deadline failures re-claim (Finding 1);
  - (b) terminal node errors stop answering 503 (Finding 5);
  - (c) slot-local refusals re-claim instead of going cold;
  - (d) no retry or re-claim after the readiness budget;
  - (e) unreachable arms removed (Finding 6).
- **Unchanged.** The Finding 4 node-wide refusals, Fallthrough's cold
  fallback, and `boot_mode=pool`'s no-reclaim behavior all stay as they are.
- **Main risk.** A slot-storm from misclassifying a request-fatal error as
  SlotLocal: bounded at 3 slots per create, introduced last, and watched
  through the per-kind `reason` label.
- **Owner decisions:** the in-place retry after a client deadline,
  re-claiming under `boot_mode=pool`, and node exclusion.
