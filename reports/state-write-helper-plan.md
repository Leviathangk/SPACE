# sbx-mgr state-write helper: inventory and plan

Read-only analysis of `crates/libs/sbx/mgr/server` at
`af09bb49a9d64192dc24b78e7d73edba1b81da4a`. No production code changed. One
throwaway DB-level test confirmed the race sequences in §3. It was run once and
deleted; it is not committed. File paths are relative to
`crates/libs/sbx/mgr/server/src/`.

## Decision summary

**Recommendation: take the smaller step first, then decide on the full
helper.** The concrete correctness risk is concentrated in a handful of
`db::update_state` callers that read a row, await I/O, and then write a target
with no expected-state predicate. The ~40 specialized compare-and-set (CAS)
writers are already guarded in SQL. The refusal tests pin them in
`db/tests/state_writer_refusal.rs` and `db/tests/cleanup_quarantine_refusal.rs`.
Consolidating those writers is a maintainability win, not a correctness fix.

1. Land the characterization gaps listed in §5 (tests only).
2. Add `update_state_from(conn, id, from: StateSet, to)`. It keeps today's
   generic quarantine and cleanup guards and returns
   `Applied { from } / Refused { found } / Missing`.
3. Convert the three highest-risk callers (pause, resident resume, create),
   one PR each, with the behavior changes listed.
4. Only then decide whether to move the specialized writers onto the
   table-driven helper in §4.

**Decisions only the owner can make:**

- **D1. What does `SandboxState::transition` mean?** Production writers
  implement about 45 edges that `state.rs` forbids (§2). Two options:
  - Treat `state.rs` as the *RPC admission* machine and keep a separate,
    tested *persisted-edge table*. This is my recommendation.
  - Widen `state.rs` to the persisted union.

  Either is defensible. The current mix is neither.
- **D2. Should Stop admission consult the state machine?** Kill does. Stop
  does not, so Stop admits `stopping` from `stopped`, `suspended`, `created`,
  `creating`, `checkpointing`, `suspending` and `failed`.
- **D3. Full helper or smaller step only?** The trade-offs are in §6.
- **D4. Should side-column policy be unified?** Transition receipts and
  `failure_reason` are cleared inconsistently across writers. Unifying them is
  a behavior change.

**Ranked findings (evidence in §3):**

| # | Risk | Finding |
|---|---|---|
| 1 | High | `pause` rollback and completion write `started` or `paused` over a row that a concurrent Stop already moved to `stopped`. Confirmed at the DB level. |
| 2 | Med-high | Resident resume without an activation policy writes `resuming` over `stopped`. It then quarantines the sandbox as `resident_resume_failed`. Confirmed at the DB level. |
| 3 | Medium | A warm-snapshot Start writes `suspended` from any non-`failed` state, including `started`. The state is never validated. |
| 4 | Medium | Create completion writes `created` or `suspended` over a concurrent Stop's `stopping`. The Stop is lost, and the row outlives the create that reported failure. Confirmed at the DB level. |
| 5 | Low-med | A full process-snapshot rollback writes `suspended` over a concurrent Stop's `stopping`. |
| 6 | Low | Template base-snapshot completion writes `paused` over `stopped` if a Stop completes during hashing. |
| 7 | Low | The auto-lifecycle Stop of a `suspended` row writes `stopped`, which `state.rs` forbids. The race window is small. |
| 8 | Info | Dead or test-only writers: `mark_starting_quarantined`, `mark_restoring_quarantined`, `rollback_starting`, `rollback_restoring`, `mark_quarantined`, `mark_stopping_superseding_activation`, `mark_deleting_superseding_activation`, `clear_cleanup_intent`. Unreachable branches: the restore-preserving branch of `request_cleanup_episode` and the non-stop/delete branch of cleanup admission. |

## 0. Ground rules that shape every design choice

- **One connection.** `run.rs` opens the only SQLite connection, and every
  service path reaches it through `Arc<Mutex<Connection>>`.
  - Statements issued under one mutex guard are atomic with respect to every
    other in-process writer.
  - A race needs a read under one guard and a write under a later guard, with
    an `.await` or a lock release in between.
  - `IMMEDIATE` versus `DEFERRED` matters only for other processes and for the
    nested-transaction rule.
- **Nested transactions.** `immediate_transaction` fails inside an open
  transaction. That is why `update_state` checks `conn.is_autocommit()`
  before opening one for `deleting`. Any helper must run both inside a
  caller's transaction and standalone.
- **What the schema enforces** (`db/sandbox-manager/20260930000000_baseline.up.sql`):
  - Triggers reject unknown state labels (`validate_sandbox_lifecycle_insert`
    and `_update`). There is no transition trigger.
  - A `CHECK` ties `restore_origin`, `restore_phase` and `restore_storage_uri`
    to `state = 'restoring'`, so every exit from `restoring` must clear them.
  - `arm_disarmed_inactivity_on_started` fires on any `UPDATE OF state` from
    `starting`, `restoring` or `resuming` to `started`.
  - `retain_route_identity_on_runtime_clear` fires when the route columns are
    cleared.
  - `guard_pending_auto_lifecycle_delete_receipt` blocks deleting a row that
    still has a pending receipt.
- **Locks.** These are per-sandbox async locks:
  - The cleanup fence (`cleanup_lock`) is taken by Stop, Delete and Kill
    admission, cold-start activation, eviction, and suspend (try-lock).
  - The wake lock is taken by resume, wake, suspend, full checkpoints and
    auto-lifecycle.
  - The artifact lock is taken by pause, suspend, snapshots and rollback.

  **Stop and Delete admission, node dispatch and row-intent completion
  (`execute_cleanup_attempt` → `complete_fenced_row_cleanup`) take only the
  cleanup fence, plus the staging lock for deletes.** Only *episode*
  completion (`finalize_successful_cleanup`) takes the artifact lock. Most of
  the races in §3 follow from that.

## 1. Inventory

Legend:
- **Txn** values:
  - `auto`: a single autocommit statement.
  - `IMM`: opens its own `IMMEDIATE` transaction.
  - `DEF`: opens `unchecked_transaction` (deferred).
  - `caller`: runs inside the caller's transaction.
- `sc/ua`: sets `state_changed_at` when the state changes and always sets
  `updated_at`.
- **Refusal semantics**: what a caller receives when the row is missing or
  the guard fails.

### 1.1 `db/state_transitions.rs`

| Writer | From predicate | To | Extra guards | Side columns | Txn | Refusal semantics |
|---|---|---|---|---|---|---|
| `update_state` | **none in SQL** except `state != 'quarantined'`, which is skipped when the target is `deleting` | any except `quarantined`/`failed` (those return `Err(InvalidQuery)`) | Read-then-check in Rust (§1.4) | sc/ua. Clears `failure_reason`. Clears the restore columns unless the target is `restoring`. Clears receipt columns unless `transition_receipt_target_state` equals the target. On `deleting`, runs `teardown_jobs::fail_active`. | `auto`; `IMM` for `deleting` in autocommit; `caller` otherwise | `SandboxStateWriteOutcome::{Applied, Missing, BlockedByQuarantine, SupersededByCleanupEpisode}`. `require_applied()` turns non-applied outcomes into `SandboxStateWriteNotApplied`. |
| `restore_checkpoint_state` | `checkpointing` | `started` or `paused` (any other target is `Err(InvalidQuery)`) | — | sc/ua; clears `failure_reason` | `auto` | `bool` |
| `recover_interrupted_pause_state` | `pausing` | `started` or `paused` | — | sc/ua; clears `failure_reason` | `auto` | `bool` |
| `begin_resident_resume_with_policy` | Rust re-read: `paused` with matching execution generation | `resuming` (via `update_state`) | Plus `update_state`'s guards | Plus installs the activation auto-lifecycle policy | `IMM` | `bool`; `false` on any mismatch (the transaction is dropped) |
| `restore_resident_resume_state` → `rollback_activation_state` | `resuming` | `started` or `paused` | — | sc/ua; clears `failure_reason`. Arms auto-lifecycle when the target is `started`. | `IMM` | `bool` |
| `update_state_superseding_activation_in_transaction` | none (delegates to `update_state`) | `stopping` or `deleting` | Refuses `stopping` when the episode entered from `quarantined` or carries a recovery fence | Deletes the cleanup episode and clears the matching `cleanup_completion`/`keep` first | `caller` (cleanup admission) | `SupersedeActivationError::{QuarantineRecoveryInProgress, StateWriteNotApplied, Db}` |
| `begin_activation_with_route_evidence` (`begin_activation` is test-only) | `state = expected` and `!= 'quarantined'`; `expected` ∈ {created, stopped, suspended} for a cold start, {suspended} for a restore | `starting` or `restoring` | No cleanup episode. No pending delete receipt at or above the generation. Intent is NULL or equal. | `failure_reason`, restore columns (`local`/`activation`), `cleanup_completion`/`keep`, node execution ids, receipts cleared, sc/ua. Also inserts the episode, claims, route evidence and receipt supersession. | `IMM` | `Ok(None)` when refused or missing (indistinguishable); `UnsupportedState` for a bad `expected` |
| `mark_start_succeeded` | `starting` | `started` | When an id is given, the cleanup episode must be armed | Clears `failure_reason`, the artifact pin and intent; sc/ua. Then the receipt, episode delete, auto-lifecycle arming and claim close. | `IMM` | `bool` |
| `complete_pool_clone` | `creating` | `suspended` | Armed `delete_pool_clone` episode with activation state `creating` | Clears intent and `failure_reason`; `state_changed_at` is unconditional; deletes the episode | `IMM` | `bool` |
| `complete_create` | `created` or `suspended` (no state write) | — | Armed episode | Clears intent and `failure_reason`; receipt; deletes the episode | `IMM` | `bool` |
| `mark_restore_succeeded` | `restoring` | `started` or `paused` | When an id is given, the episode must be armed | Clears `failure_reason`, intent and restore columns; sc/ua; receipt, episode delete, arming, claim, event | `IMM` | `bool` |
| `begin_suspend` | `paused` | `suspending` | Intent must be NULL | Restore columns cleared. `suspend_predecessor_artifact_id` ← current pin when it is a full suspend pin. Pin cleared. `failure_reason` cleared. sc/ua. | `auto` | `bool` |
| `begin_teardown_suspend` | `paused` | `suspending` | Teardown lease/operation in the same transaction. **No intent check, unlike `begin_suspend`.** | Same as `begin_suspend` | `IMM` | `Err(QueryReturnedNoRows)` |
| `rollback_suspending_to_paused` | `suspending` | `paused` | — | Pin ← predecessor; clears intent and `failure_reason`; sc/ua | `auto` | `bool` |
| `rollback_{starting,restoring}_episode` → `rollback_activation_episode` | `starting` or `restoring` | caller's state (not `failed` or `quarantined`) | Armed episode with that id | Clears `failure_reason`, intent and restore columns; sc/ua; deletes the episode | `IMM` | `bool`; `RollbackActivationEpisodeError` for a bad target |
| `mark_{suspending,starting,restoring}_quarantined` | see §1.3 `mark_quarantined_from_state` | `quarantined` | — | — | — | `Result<bool, MarkQuarantinedError>`. Only the `suspending` variant has production callers. |
| `mark_teardown_suspend_backup_permanently_failed`, `mark_cross_node_restore_staging_failed` | `suspending` / `restoring` | (no state write) | Artifact pin / — | `failure_reason`, `updated_at` | `auto` | `bool` |

### 1.2 `db/cleanup_episodes.rs`

| Writer | From predicate | To | Extra guards | Side columns | Txn | Refusal semantics |
|---|---|---|---|---|---|---|
| `rearm_prepared_clone_disposal` | `quarantined` | `cleanup_required` | Its own `delete_pool_clone` episode exists | intent ← `delete_pool_clone`/0; `state_changed_at` unconditional; episode re-requested | `IMM` | `bool` |
| `request_cleanup_episode` | `NOT IN ('deleting','quarantined')` | `cleanup_required` | Episode id matches and is not yet requested; row intent equals the episode intent | `failure_reason` ← message. Restore columns cleared unless the episode has a recovery fence (unreachable, see F-d). sc/ua. Episode → requested. Event. | `IMM` | `CleanupEpisodeRequestOutcome::{Requested, AlreadyRequested, Superseded}`; `PhaseUpdateInvariant` |
| `record_cleanup_episode_failure` | `cleanup_required` or `cleanup_failed` | `cleanup_failed` | Requested episode; row intent equals the episode completion | sc/ua; episode `last_cleanup_error` and attempts; event | `IMM` | `bool` (true when the episode updated, even if the state write missed) |
| `complete_cleanup_episode_for_job` → `complete_state_cleanup` → `apply_cleanup_state` | **SQL is `WHERE id = ?` only.** The guard is `cleanup_episode_current` (cleanup state, intent, requested) checked earlier in the same transaction. | the completion's target: `created`, `stopped`, `suspended`, `quarantined`; or the row is deleted | Quarantine-recovery match; suspend artifact check | Clears runtime fields and route identity (keeping the generation and pod for stop/suspend proof), intent, restore columns and the predecessor. Receipt set or kept. `failure_reason` ← the episode message. sc/ua. Then quarantine episode, recovery retirement, claim close, job finish and events. | `IMM` | `Ok(None)` when superseded; `CompleteCleanupEpisodeError` |
| `complete_row_intent_cleanup_for_job` → `apply_cleanup_state` | Rust `completion.owns_row_intent(row)`: `stopping` for MarkStopped; `suspending` for MarkSuspended; `deleting` for DeleteRow; `cleanup_*` with the persisted intent; `cleanup_required` with NULL intent for Quarantine | `stopped`, `suspended`, `created`, `quarantined` (missing artifact or Quarantine); or the row is deleted | No episode; delete-receipt ownership | As `apply_cleanup_state`; artifact upsert; claim close; job | `IMM` | `CompleteRowIntentCleanupDisposition::{Applied, Quarantined, Superseded}`. **When `job_id` is `None`, a non-owned intent returns `Applied`.** |
| `quarantine_cleanup_episode_for_job` | Rust: `cleanup_required` or `cleanup_failed`; SQL: `state = observed` plus the intent tuple | `quarantined` | Episode id plus a reason code | `failure_reason`; clears intent and restore columns; sc/ua; claim, quarantine episode, episode delete, job, event | `IMM` | `Result<bool, MarkQuarantinedError>` |
| `mark_cleanup_required` | **any except `quarantined`** (Rust pre-read plus SQL) | `cleanup_required` | — | `failure_reason`, intent, restore columns cleared, sc/ua. **No event.** | `auto` | `Ok(())` even when quarantined or missing |
| `mark_cleanup_required_if_no_intent[_in_state]` | any except `quarantined`; optionally `state = expected` | `cleanup_required` | Intent must be NULL | Same as above, plus an event | `auto` | `bool` |
| `mark_cleanup_failed` | any except `quarantined` | `cleanup_failed` | Caller checks `owns_row_intent` under the same guard (`cleanup.rs::mark_async_cleanup_failed`) | `failure_reason` and intent; sc/ua; **restore columns not cleared** (the schema `CHECK` backstops this); event | `auto` | `bool` |
| `insert_cleanup_episode`, `set_cleanup_intent`, `retire_stale_armed_cleanup_episode`, `clear_cleanup_intent` | (no state write) | — | `set_cleanup_intent` requires no episode | `cleanup_completion`/`keep` | various | various |

### 1.3 Other `db/` modules and the delete-receipt store

| Writer | From predicate | To | Extra guards | Side columns | Txn | Refusal |
|---|---|---|---|---|---|---|
| `eviction.rs::claim_suspended_sandbox_for_eviction` | `suspended` | `deleting` | Pin equals the expected pin (or NULL); optional `updated_at` version | sc/ua; `teardown_jobs::fail_active` | `IMM` | `bool` |
| `claim_failed_restoring_sandbox_for_eviction` | `restoring` | `deleting` | Non-blank `failure_reason`; no pin; `updated_at` version | Restore columns cleared; `state_changed_at` unconditional | `IMM` | `bool` |
| `claim_quarantined_sandbox_for_eviction` | `quarantined` | `deleting` | `updated_at` version | sc/ua | `IMM` | `bool` |
| `claim_stopped_sandbox_for_eviction` | `stopped` | `deleting` | `updated_at` version | sc/ua | `IMM` | `bool` |
| `claim_teardown_suspend_failed_sandbox_for_eviction` | `suspending` | `deleting` | Terminal failure reason, pin and `state_changed_at` witness | sc/ua | `IMM` | `bool` |
| `local_artifacts.rs::pin_suspended_artifact_for_cleanup` | `suspending` | `cleanup_required` | — | Pin, predecessor cleared, intent ← `mark_suspended`, `failure_reason` cleared | `IMM` | `Err(QueryReturnedNoRows)` |
| `publish_restored_suspended_artifact` | `restoring` with no pin | `suspended` | — | Restore columns, pin. **`failure_reason` not cleared**, unlike its siblings. Transfer scope retired. | `IMM` | `Err(QueryReturnedNoRows)` |
| `publish_restored_suspended_without_artifact` | `restoring` with no pin | `suspended` | — | Restore columns, pin, `failure_reason` cleared | `IMM` | `Err(QueryReturnedNoRows)` |
| `publish_restored_disk_as_new` | `restoring` | `stopped` | — | As above, plus `clear_runtime_fields` | `IMM` | `Err(QueryReturnedNoRows)` |
| `publish_stopped_artifact` | `stopping`; or `cleanup_*` with `mark_stopped`; or `stopped` with no pin | `stopped` | **No episode check** | Disk pin, runtime and route fields, intent, `failure_reason`; claim close | `IMM` | `Err(QueryReturnedNoRows)` |
| `quarantine.rs::mark_quarantined_inner` (`mark_quarantined` is test-only; `mark_quarantined_from_state` and `mark_quarantined_best_effort` are the production entry points) | Rust pre-read `previous`, optional `== expected`, then `previous.transition(Quarantined)`; SQL `state = previous` | `quarantined` | Reconfirmation reuses the episode and resets retry state only when the classification changed, legacy recovery owns it, and no lease is live | `failure_reason`; predecessor, restore and receipt columns cleared; sc/ua; claim close; quarantine state and history; event | `DEF` (the read happens before the transaction) | `Result<bool, MarkQuarantinedError>` (`Transition` for `failed`, `UnknownCurrentState`) |
| `recover_pause_failed_quarantine` | `quarantined` | `started` or `paused` | Exact `pause_failed` quarantine id, source and reason | `failure_reason` cleared; history plus retirement | `IMM` | `Ok(false)` when no match; `PauseFailedRecoveryError` invariants |
| `insert_quarantined_sandbox_in_transaction` | INSERT | `quarantined` | — | `failure_reason`; new episode | `caller` | `NonQuarantinedInsert` |
| `quarantine_recovery.rs::publish_orphan_quarantine_recovered_artifact` | `restoring` with no pin, at the matching generation | `suspended` | `ExactClaimFence` (live lease, no cleanup) | Restore columns, `failure_reason`, pin, `quarantine_recovery` receipt; retires quarantine state and transfer scope | `IMM` | `bool` |
| `adopt_quarantine_cleanup_episode` | `quarantined` with a full suspend pin | `cleanup_required` | `ExactClaimFence` | intent ← `mark_suspended`, generation, `failure_reason`; releases the lease; inserts a *requested* episode with a recovery fence | `IMM` | `QuarantineAdoptionOutcome::{Adopted, FenceChanged}` |
| `mark_cross_node_restore_staging_failed_under_recovery_claim`, `set_orphan_quarantine_recovery_generation` | `restoring` | (no state write) | `ExactClaimFence` | `failure_reason` / generation | `auto` | `bool` |
| `startup_recovery.rs::settle_startup_resident` | Rust: `started`, `paused`, `resuming` or `checkpointing`, plus `startup_resident_witness_current` (same version, no cleanup ownership, no transfer scope) | `started`, `paused` or `quarantined` | Witness | `failure_reason`; receipts kept only for a matching healthy target; **`state_changed_at` unconditional**; quarantine episode and claim | `IMM` | `bool` |
| `recover_interrupted_startup_states` | composes `request_cleanup_episode`, `mark_cleanup_required` and `update_state(Suspended)` from `restoring` | — | — | — | per call | `RecoverStartupStatesError` |
| `transfer.rs::complete_import` | `creating` | `stopped` or `suspended` | Armed episode | Clears intent; `state_changed_at` unconditional; deletes the episode | `IMM` | `bool` |
| `sandboxes.rs::insert_sandbox` (via `insert_sandbox_with_cleanup_episode_and_route_evidence`, `transfer`, `cache_cleanup`, `insert_recovered_suspended_sandbox_in_transaction`) | INSERT | the caller's state (`creating`, `cleanup_required`, `suspended`, `quarantined`) | — | — | `caller`/`IMM` | `Err` on conflict |
| `restore_adopt_orphan_or_insert_sandbox_in_transaction` | INSERT … `ON CONFLICT DO UPDATE WHERE state IN ('quarantined','restoring')`, an `orphaned_sandbox_row_missing` quarantine marker, and the exact synthetic-orphan column tuple | `restoring` | Synthetic-orphan fence | Rewrites nearly every column | `caller` | `ExistingRowNotSyntheticOrphan`, `NonRestoringState` |
| `auto_lifecycle_delete_receipts/db.rs` | (no state writes) | — | — | Route evidence, block reason, auto-lifecycle JSON | — | — |

Writers that touch only non-state columns (`network_policy.rs`, `pool.rs`,
`auto_lifecycle.rs`, most of `sandboxes.rs`) are out of scope. Several of
them guard on `state` (for example `update_space_api_route_identity` requires
`started` or `paused`). They should keep that guard but do not need the
helper.

### 1.4 `update_state`'s generic guard, exactly

All three rules are evaluated in Rust after `current_state_label`, under the
caller's mutex guard.

1. **Quarantine.** If the previous state is `quarantined` and
   `quarantined.transition(target)` fails, the outcome is
   `BlockedByQuarantine`. In practice only `deleting` passes.
2. **Cleanup ownership.** The outcome is `SupersededByCleanupEpisode` when
   any of the following holds:
   - A cleanup episode exists whose completion does not delete the row, or
     that has been requested.
   - The previous state is `deleting`, `cleanup_required` or `cleanup_failed`,
     the target is not `deleting`, and a `delete_row` intent is current.
   - The previous state is `stopping`, `cleanup_required` or `cleanup_failed`,
     and the target is one of `started`, `paused`, `pausing`, `resuming` or
     `checkpointing`.
3. **Everything else applies.**
   - **There is no from-state predicate.** An armed `delete_row` create
     episode deliberately does *not* block, so create completion depends on
     this.
   - Nothing protects against `stopped`, `created`, `suspended`, `starting`,
     `restoring` or `failed` as previous states, and nothing protects targets
     such as `created`, `suspended` or `stopped` from overwriting `stopping`.

### 1.5 Non-test callers of `update_state` / `update_state_best_effort`

| Caller | Writes | Read→write gap | Locks held | Notes |
|---|---|---|---|---|
| `service/grpc_lifecycle/pause_resume.rs::pause` | `pausing`; then `started` (rollback after guest-prep failure); `started` best-effort (node connect failure); `paused` (completion) | Guest snapshot preparation RPC; node pause RPC | artifact | Checks `transition(Pausing)` on the read only |
| `pause_resume.rs::pause_with_reconciliation` (also used by suspend) | `started` best-effort | Node RPCs and reconcile probes | Caller's locks | Pause RPC: artifact lock only. Suspend: cleanup fence plus wake and artifact locks. |
| `service/grpc_lifecycle/suspend.rs` (auto-pause before suspend) | `pausing`, `started` (rollback), `paused` | Guest prep, node pause | **cleanup fence**, wake, artifact | Protected from Stop and Delete |
| `service/auth_lookup.rs::resume_after_wake_lock` (no-policy branch) | `resuming` | `apply_network_policy_before_wake` (network, retried) | wake | The policy branch uses `begin_resident_resume_with_policy`, which is safe |
| `service/create.rs::create_with_id` | `created`; then `suspended` (base-snapshot mode) | Cache root, base-snapshot reflinks, node memory-policy RPC | none | Relies on rule 3 to pass the armed `delete_row` episode |
| `service/start.rs::try_start_from_warm_snapshot` | `suspended` when the state is not already `suspended` | Template resolution, reflinks | none (the cleanup fence is only taken later, on the cold path) | Only `failed` is rejected beforehand |
| `service/grpc_spaced_process_snapshots.rs` (full rollback) | `suspended` | Btrfs rollback RPC, reflinks | artifact | Verifies `stopped` after its own stop, then awaits |
| `service/grpc_state_snapshot.rs::create_template_base_snapshot` | `checkpointing` (check and write under one guard); `paused` and best-effort `paused` | Capture, hash, publish | wake | Template-owned sandboxes only |
| `snapshot_scheduler.rs::do_checkpoint` | `checkpointing` | **None.** Check and write under one guard. | wake and artifact (full) | Rollback uses the `restore_checkpoint_state` CAS |
| `service/reconcile.rs::run_due_auto_lifecycle` (Stop on `suspended`) | `stopped` | Synchronous only, separate guards | wake | `suspended → stopped` is forbidden by `state.rs` |
| `service/cleanup.rs::admit_lifecycle_cleanup_fenced` | `stopping` or `deleting` via the superseding writer; the `else` branch (`update_state`) is unreachable | None (one transaction under the cleanup fence) | cleanup | Never consults `state.rs` (D2) |
| `db/startup_recovery.rs::recover_interrupted_startup_states` | `suspended` from `restoring` | — | Startup, before admission | |
| `service/recovery.rs::recover_interrupted_create` / `recover_unlaunched_start` | `created` / `stopped` or `created` | — | Startup | `starting → stopped/created` is forbidden by `state.rs` |
| `db/state_transitions.rs` (internal) | `begin_resident_resume_with_policy`, superseding writer, `deleting` re-entry | — | — | |

## 2. Transition table: persisted union vs `SandboxState::transition`

✓ means `can_transition_to` allows the edge; ✗ means it forbids it.
"Admission" is cleanup admission for Stop or Delete, through the superseding
writer.

| From | Targets written by production code |
|---|---|
| creating | created ✓ (`update_state`: create, startup); suspended ✗ (`complete_pool_clone`, `complete_import`); stopped ✗ (`complete_import`); cleanup_required ✗ (`request_cleanup_episode`); stopping ✗ and **deleting ✗** (admission superseding the create episode); quarantined ✓ |
| created | starting ✓ (`begin_activation`); suspended ✗ (create base-snapshot, warm start, rollback); stopping ✗ (Stop); deleting ✓ |
| starting | started ✓ (`mark_start_succeeded`); created/stopped/suspended ✗ (`rollback_starting_episode`, `recover_unlaunched_start`); cleanup_required ✗ (`request_cleanup_episode`, startup `mark_cleanup_required`); stopping ✗ (Stop); deleting ✓ |
| started | pausing ✓, checkpointing ✓, stopping ✓, deleting ✓, quarantined ✓; paused ✗ (`settle_startup_resident`); suspended ✗ (warm start, unvalidated) |
| pausing | paused ✓; started ✗ (pause rollback, `recover_interrupted_pause_state`); stopping ✗ (Stop); deleting ✓, quarantined ✓ |
| paused | resuming ✓, checkpointing ✓, suspending ✓, stopping ✓, deleting ✓; started ✗ (`settle_startup_resident`) |
| resuming | started ✓ (`restore_resident_resume_state`); paused ✗ (resume rollback, `settle_startup_resident`); deleting ✓, quarantined ✓ |
| checkpointing | started ✓, paused ✓ (`restore_checkpoint_state`, template snapshot, settle); stopping ✗ (Stop); deleting ✓, quarantined ✓ |
| suspending | suspended ✓, quarantined ✓, deleting ✓ (eviction); paused ✗ (`rollback_suspending_to_paused`); cleanup_required ✗ (`pin_suspended_artifact_for_cleanup`, startup); cleanup_failed ✗; stopping ✗ (Stop) |
| suspended | starting ✓, restoring ✓, deleting ✓; stopped ✗ (auto-lifecycle Stop); stopping ✗ (Stop) |
| restoring | started ✓, paused ✓, deleting ✓; suspended ✗ (episode rollback, `publish_restored_suspended_*`, `publish_orphan_…`, startup); stopped ✗ (`publish_restored_disk_as_new`); cleanup_required ✗ (three writers); stopping ✗ (Stop) |
| stopping | stopped ✓ (`complete_row_intent_cleanup`, `publish_stopped_artifact`); deleting ✓; cleanup_required ✗ (startup); cleanup_failed ✗ (`mark_cleanup_failed`); **created/suspended/started/paused/resuming ✗ (races in §3)** |
| stopped | starting ✓, deleting ✓; suspended ✗ (rollback, warm start); stopping ✗ (Stop) |
| deleting | (row deleted); cleanup_required ✗ (startup); cleanup_failed ✗ (`mark_cleanup_failed`) |
| cleanup_required | cleanup_failed ✓, quarantined ✓, deleting ✓; created/stopped/suspended ✗ (`complete_cleanup_episode`, `complete_row_intent_cleanup`, `publish_stopped_artifact`) |
| cleanup_failed | cleanup_required ✓, quarantined ✓, deleting ✓; created/stopped/suspended ✗ (same writers) |
| quarantined | deleting ✓, quarantined ✓ (reconfirm); cleanup_required ✗ (`adopt_quarantine_cleanup_episode`, `rearm_prepared_clone_disposal`); started/paused ✗ (`recover_pause_failed_quarantine`); restoring ✗ (`restore_adopt_orphan_…`) |
| failed | deleting ✓; stopping ✗ (Stop); `update_state` accepts any other non-quarantine target for `failed` rows (confirmed: `failed → started` applies) |

**Writers permit what the state machine forbids:** about 45 edges.

- Most are deliberate compensation or recovery edges.
  - Episode rollback returns the row to its entry state.
  - Cleanup completion settles a `cleanup_*` row.
  - Publication settles a `restoring` row.
  - Quarantine recovery exits `quarantined`.

  `state.rs` simply does not model them. Its `CleanupRequired`/`CleanupFailed`
  edges cover only the retry loop and delete.
- The edges that are **not** deliberate are the §3 races:
  - `stopping/stopped → started/paused/resuming/created/suspended`.
  - The unvalidated warm start (`started → suspended`).
  - Stop admission from non-running states. D2 decides whether that is
    intended.

**The state machine permits what no writer implements:**

- `started → suspending`. Suspend always pauses first.
- Most of the `X → quarantined` edges. Production quarantines only from
  `pausing`, `stopping`, `resuming`, `checkpointing`, `suspending`, and the
  cleanup and startup-resident states.
- No writer targets `failed`. `failed` is read-only legacy vocabulary on this
  node, consistent with `update_state` refusing it.

**Where `transition()` is actually consulted:**

| Path | Check |
|---|---|
| Pause | `transition(Pausing)` |
| Kill | `transition(Stopping)` |
| Resident resume | `transition(Resuming)` |
| Cold start | `transition(Starting)` |
| Create-from-restore | `transition(Restoring)` |
| Full snapshot and template snapshot | `transition(Checkpointing)` |
| `mark_quarantined_inner`, `quarantine_cleanup_episode_for_job` | `transition(Quarantined)` |
| `generic_writer_transition_blocked` | quarantine only |
| Auto-lifecycle delete gating | `can_transition_to(Deleting)` |

Stop, Delete, every completion and every recovery path skip it.

## 3. The unguarded callers

**Relying on the missing expected-state check** (an SM-derived check would
break them, so they need explicit from-sets):
- create (`creating → created`, `created → suspended`)
- warm start (`{created, stopped} → suspended`)
- full process-snapshot rollback (`stopped → suspended`)
- auto-lifecycle Stop (`suspended → stopped`)
- startup `restoring → suspended`
- `recover_unlaunched_start` (`starting → stopped/created`)
- pause rollback (`pausing → started`)
- cleanup admission (Stop from any state)
- the fixture `db/tests.rs::application_state_write_updates_only_state_changed_at`
  (`started → paused`)

**Latent races**, ranked. "Confirmed" means a throwaway DB test reproduced the
exact writer sequence on the test schema: `update_state` returned `Applied`
for the stale target.

1. **`pause` vs Stop: high.** The sequence:
   1. Pause checks `started` and writes `pausing` under the artifact lock.
   2. Pause awaits guest snapshot preparation.
   3. Stop takes the cleanup fence (not the artifact lock) and admits
      `pausing → stopping`, which `update_state` rule 3 allows.
   4. Stop kills the VM, and `complete_row_intent_cleanup(MarkStopped)`
      settles `stopped`.
   5. The guest prep now fails *because* the VM is gone, and the rollback
      writes `started`.

   Confirmed: `stopped → started` is `Applied`, and the completion path's
   `stopped → paused` is also `Applied`. The race triggers itself, since
   Stop's kill is what makes the prep fail. The result is a `started` row
   with cleared runtime fields and no VM. Startup recovery would quarantine
   it at the next restart. I did not establish whether any periodic pass
   repairs it sooner. The suspend path's identical writes are safe because
   suspend holds the cleanup fence.
2. **Resident resume without an activation policy vs Stop: medium-high.**
   The sequence:
   1. Resume validates `paused → resuming` on the read.
   2. It awaits `apply_network_policy_before_wake`, a network round trip with
      retries.
   3. It writes `resuming` with no predicate. Stop does not take the wake
      lock, so a Stop that completes in step 2 leaves `stopped`.

   Confirmed: `stopped → resuming` is `Applied`. The node resume then fails,
   the VM is unobservable, and `finish_resident_resume_after_rpc_error`
   quarantines the sandbox (`resident_resume_failed`), which needs operator
   attention. The policy branch (`begin_resident_resume_with_policy`)
   re-checks inside `IMMEDIATE` and is safe.
3. **Warm-snapshot Start writes from an unvalidated state: medium.** Impact
   is high, likelihood low.
   - `start_sandbox_with_authority` rejects only `failed` before
     `try_start_from_warm_snapshot`.
   - It writes `suspended` whenever the row is not already `suspended`. The
     cold path's `transition(Starting)` runs later.
   - A `Start{from_warm: true}` on a `started` or `paused` row rewrites it to
     `suspended` and proceeds to a checkpoint restore. Confirmed that
     `started → suspended` and `stopping → suspended` apply.
   - The callers are the operator CLI and the template pool manager.
4. **Create vs Stop: medium.** Create holds no lock between the insert and
   `complete_create`.
   1. Stop's superseding admission deletes the create episode and writes
      `creating → stopping`.
   2. Create's `update_state(Created)` applies over `stopping` (confirmed).
      In base-snapshot mode, `update_state(Suspended)` applies too
      (confirmed).
   3. `complete_create` returns `false`, so the create RPC reports
      `Aborted`. The row nevertheless persists as `created` or `suspended`
      with no episode.
   4. Stop's cleanup completion no longer owns the intent, so it does
      nothing.

   The Stop is silently lost, and the row survives a create that reported
   failure.
5. **Full process-snapshot rollback vs Stop: low-medium.** Rollback verifies
   `stopped`, then awaits the btrfs rollback and reflinks under the artifact
   lock, then writes `suspended`. A Stop admitted in that window
   (`stopped → stopping`) is overwritten. A concurrent Start is safe: rule 3
   blocks it on the armed `mark_stopped` episode.
6. **Template base-snapshot completion vs Stop: low.** A Stop that completes
   during capture and hash leaves `stopped`, and the completion writes
   `paused`. This only affects template-manager sandboxes.
7. **Auto-lifecycle Stop of `suspended`: low.** The re-read and the write are
   synchronous with no await in between. The competing writers (activation,
   eviction) either share the wake lock or are blocked by the guard rules.
   The remaining issue is the forbidden edge itself (D1).

**Not races:** `do_checkpoint` and template snapshot begin (check and write
under one guard), cleanup admission (one transaction under the cleanup
fence), the suspend auto-pause (cleanup fence), startup recovery (runs before
admission), and pause begin (no await between check and write; a concurrent
`stopping` is caught by rule 3).

Other findings from the inventory:

- **F-d.** `request_cleanup_episode`'s preserve-restore-columns branch needs
  an *armed* episode with a quarantine-recovery fence. Only
  `adopt_quarantine_cleanup_episode` creates fenced episodes, and it inserts
  them already *requested*, so the branch is unreachable. If it were reached
  with restore columns set, the schema `CHECK` would reject the
  `cleanup_required` write.
- **F-e. Side-column drift.**
  - `update_state` invalidates receipts whose target differs from the new
    state, but `begin_suspend`, `rollback_suspending_to_paused`, the eviction
    claims and the publication writers leave them.
  - `publish_restored_suspended_artifact` keeps a stale `failure_reason`
    while its two siblings clear it.
  - `settle_startup_resident`, `complete_import`, `complete_pool_clone`,
    `rearm_prepared_clone_disposal` and four of the five eviction claims set
    `state_changed_at` unconditionally.
  - Events are emitted by `mark_cleanup_required_if_no_intent` but not by
    `mark_cleanup_required`, and by `mark_restore_succeeded` but not by
    `mark_start_succeeded`.
- **F-f.** `complete_row_intent_cleanup_for_job` reports `Applied` for a
  non-owned intent when no job id is supplied.

## 4. Proposed API

One module, `db/state_write.rs`, owns the persisted-edge table and the CAS.
Writers keep their public names and companion statements. Only the
`sandboxes.state` statement moves.

```rust
/// Bitset over the 18 lifecycle states; `SandboxState::all()` order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StateSet(u32);

/// One persisted edge family. Constants only: call sites cannot invent edges.
pub(crate) struct Edge {
    pub name: &'static str,          // "pause_rollback", "evict_stopped", ...
    pub from: StateSet,
    pub to: SandboxState,
    /// Edges out of `quarantined` must carry a quarantine authority guard.
    pub quarantine_exit: bool,
}
pub(crate) mod edges { /* pub const PAUSE_ROLLBACK: Edge = ...; one per row of §2 */ }

pub(crate) enum Guard<'a> {
    // Cleanup ownership.
    GenericCleanupSupersession,                 // §1.4 rules 1-2, as SQL
    NoCleanupIntent,                            // completion and keep IS NULL
    CleanupIntent(CleanupCompletion, bool),
    NoCleanupIntentOr(CleanupCompletion, bool), // begin_activation's rule
    NoCleanupEpisode,
    ArmedEpisode(SandboxCleanupEpisodeId),
    ArmedEpisodeFor { id: SandboxCleanupEpisodeId, activation: SandboxState, completion: CleanupCompletion },
    // Fences.
    NoPendingDeleteReceipt(Option<ExecutionGeneration>),
    RowVersion(&'a str),                        // updated_at
    StateChangedAt(&'a str),
    ArtifactPin(Option<&'a str>),
    FailureReason(TextMatch<'a>),               // Eq / NonBlank
    QuarantineClaim(&'a ExactClaimFence),       // existing fence fragment and named binds
}

pub(crate) enum Column<'a> {
    FailureReason(Keep<Option<&'a str>>),       // explicit: no default (F-e)
    CleanupIntent(Keep<Option<(CleanupCompletion, bool)>>),
    Receipt(ReceiptPolicy<'a>),                 // KeepIfTargetMatches | Keep | Clear | Set(..)
    ArtifactPin(PinUpdate<'a>),
    SuspendPredecessor(PredecessorUpdate),
    RuntimeFields(RuntimeClear),                // apply_cleanup_state's retention rule
    Raw(&'static str),                          // writer-owned SET fragment, static text only
}

pub(crate) struct StateWrite<'a> {
    pub id: &'a str,
    pub edge: &'static Edge,
    pub guards: &'a [Guard<'a>],
    pub columns: &'a [Column<'a>],
}

#[must_use]
pub(crate) enum StateWriteOutcome {
    Applied { from: SandboxState },
    Refused { found: FoundRow, refusal: Refusal },
    Missing,
}
pub(crate) enum Refusal { FromState, Quarantined, CleanupOwned, Guard(&'static str) }

/// Never opens or commits: `conn` must be inside a transaction.
pub(crate) fn write_state(conn: &Connection, w: &StateWrite<'_>) -> rusqlite::Result<StateWriteOutcome>;
/// Opens IMMEDIATE when `conn.is_autocommit()`, as `update_state` does today.
pub(crate) fn write_state_standalone(conn: &Connection, w: &StateWrite<'_>) -> rusqlite::Result<StateWriteOutcome>;
```

**Mechanics.** `write_state` runs four steps:

1. Read `state`, the intent, the episode summary and every guarded column.
   A missing row returns `Missing`.
2. Evaluate the from-set and the guards in Rust. A failure returns
   `Refused { found, refusal }`.
3. Execute one `UPDATE … WHERE id = ? AND state IN (<from>) AND <guards>`
   carrying the same predicates.
4. Treat `changed == 0` after step 2 passed as an invariant error. That
   signals SQL/Rust parity drift, which a table-driven parity test covers in
   the style of the existing `SandboxState::all()` filter-parity tests.

This design has two consequences:
- It returns `from` for events, which today needs a separate
  `get_sandbox`/`current_state_label`.
- Step 3 keeps every write a true CAS, so it stays correct if a second
  connection ever appears.

**Defaults derived from the edge:**
- sc/ua.
- Restore columns cleared when `to != restoring`. The schema `CHECK` makes
  this mandatory, so it should not be optional.
- `teardown_jobs::fail_active` whenever `to == deleting`. Today
  `update_state` and all five eviction claims do this, and they are the only
  writers of `deleting`.

Everything else is explicit.

**Quarantine refusals.** No from-set contains `quarantined` unless the
`Edge` sets `quarantine_exit`. A `debug_assert` and a table test require such
edges to carry `QuarantineClaim`, `RowVersion` or an exact-quarantine guard. A
refusal on a quarantined row returns `Refused { refusal: Quarantined }`, which
maps one-to-one onto today's `BlockedByQuarantine`.

**Cleanup-ownership refusals.** Writers relate to cleanup in three ways:
- **The cleanup owner itself.** Activation completion, rollback and cleanup
  completion pass `ArmedEpisode*`, `CleanupIntent` or a current-episode
  guard.
- **Must yield to cleanup.** Generic lifecycle writes pass
  `GenericCleanupSupersession` or `NoCleanupIntent`. The refusal maps onto
  `SupersededByCleanupEpisode`.
- **Supersedes cleanup.** Stop and Delete admission stay a composed function:
  delete the episode and clear the intent, then call `write_state` in the
  same transaction.

**Multi-statement transactions.** The writer keeps ordering control:
1. Pre-statements.
2. `write_state`.
3. Companion statements on `Applied`: episode delete or insert, claims,
   receipts, quarantine episodes, jobs.
4. Commit.
5. Events.

`mark_sandbox_dirty` stays at the service layer.

**Mapping today's outcomes:**

| Today | Becomes |
|---|---|
| `bool` | `outcome.is_applied()` |
| `Err(QueryReturnedNoRows)` | `outcome.require_applied()?` |
| `SandboxStateWriteOutcome` | `From` impl |
| `CleanupEpisodeRequestOutcome::Superseded` | `Refused { .. }` |

Public signatures can stay unchanged until each caller is ready to use
`found`.

### 4.1 Three representative conversions

**(a) Generic caller with a from-set, a behavior change: pause rollback in
`service/grpc_lifecycle/pause_resume.rs::pause`.**

```rust
// Before
require_state_write(
    crate::db::update_state(&db, &sandbox.id, SandboxState::Started),
    "roll back failed pause preparation",
)
// After: edges::PAUSE_ROLLBACK = { from: {Pausing}, to: Started }
require_state_write(
    crate::db::update_state_from(&db, &sandbox.id, &crate::db::edges::PAUSE_ROLLBACK),
    "roll back failed pause preparation",
)
```

`update_state_from` is `write_state_standalone` with
`GenericCleanupSupersession`, `FailureReason(Set(None))` and
`Receipt(KeepIfTargetMatches)`. That is exactly today's `update_state` side
effects, plus the from-set. A `stopped` row now returns `Refused { found:
stopped }`, which `require_state_write` already maps to `Status::aborted`.
The behavior change is limited to finding 1 in §3.

**(b) Specialized CAS, behavior-preserving:
`db/eviction.rs::claim_stopped_sandbox_for_eviction`.**

```rust
pub fn claim_stopped_sandbox_for_eviction(conn: &Connection, id: &str, expected_updated_at: &str) -> Result<bool> {
    let tx = immediate_transaction(conn)?;
    let outcome = write_state(&tx, &StateWrite {
        id,
        edge: &edges::EVICT_STOPPED,                    // {Stopped} -> Deleting (fail_active implied)
        guards: &[Guard::RowVersion(expected_updated_at)],
        columns: &[Column::Receipt(ReceiptPolicy::Keep)], // today's statement leaves receipts
    })?;
    tx.commit()?;
    Ok(outcome.is_applied())
}
```

The unconditional `state_changed_at` becomes the conditional CASE, which is
equivalent because the from-set excludes the target. This conversion is
pinned by `stopped_eviction_claim_binds_the_state_and_the_observed_version`.

**(c) Multi-statement writer with cleanup ownership and an event:
`db/cleanup_episodes.rs::request_cleanup_episode`.**

```rust
let tx = immediate_transaction(conn)?;
let Some(episode) = cleanup_episode(&tx, sandbox_id)? else { tx.commit()?; return Ok(Superseded) };
// unchanged: id mismatch -> Superseded; requested_at -> AlreadyRequested/Superseded
let outcome = write_state(&tx, &StateWrite {
    id: sandbox_id,
    edge: &edges::REQUEST_ACTIVATION_CLEANUP, // today: ALL - {Deleting, Quarantined} -> CleanupRequired
    guards: &[Guard::CleanupIntent(episode.completion, episode.keep_filesystem)],
    columns: &[Column::FailureReason(Set(Some(failure_message)))], // restore cleared by default
})?;
let StateWriteOutcome::Applied { from } = outcome else { tx.commit()?; return Ok(Superseded) };
// unchanged: episode phase UPDATE + PhaseUpdateInvariant
tx.commit()?;
emit_sandbox_state_event(sandbox_id, SandboxEventType::CleanupRequested, Some(from.as_str()), ..);
```

This is behavior-preserving and drops the unreachable branch (F-d). A later
PR can narrow the from-set to `{episode.activation_state}`, which would be a
listed behavior change.

## 5. Migration plan

Each PR is small. "Pins" names existing tests that must stay green. "New"
names the characterization to add first.

| PR | Content | Behavior | Pins / New |
|---|---|---|---|
| 0 | **Tests only.** (i) An `update_state` outcome matrix: all 18×18 `(from, to)` pairs, with and without a `delete_row` intent, an armed create episode, a requested episode, and `quarantined`. (ii) DB-level race sequences for §3 findings 1, 2, 4 and 5, asserting today's `Applied` with a "known race" note. (iii) An accepted-shape whole-row diff per specialized writer, reusing `state_writer_refusal::database_rows`, so later moves are provably column-identical. (iv) Direct tests for `restore_checkpoint_state` (none today), `update_state_superseding_activation_in_transaction` (none), `settle_startup_resident` (one), `restore_adopt_orphan_or_insert_sandbox` (one), `adopt_quarantine_cleanup_episode` and `pin_suspended_artifact_for_cleanup` (two each). (v) Stop admission from every source state, unless #271 already covers it. | none | New, as listed |
| 1 | `db/state_write.rs`: `StateSet`, the `Edge` constants (data only), and a table test asserting that (a) the edge table equals the §2 union and (b) its difference from `can_transition_to` equals a checked-in allowlist, so any new disagreement fails CI. Add `update_state_from` and `StateWriteOutcome`. No callers yet. | none | New table and parity tests |
| 2 | Convert `pause` (begin, rollback, best-effort `started`, completion) and `pause_with_reconciliation` | **Changes:** `stopped`, `created`, `suspended` and other non-`pausing` rows refuse `started`/`paused`; `pausing` must be the found state. Flips PR0(ii) finding 1. | `pause_refuses_rows_that_cannot_enter_pausing_before_writing_state`, `confirmed_node_pause_reports_paused_and_carries_the_execution_fence`, `teardown_suspend_pause_failure_recovery_allows_same_generation_retry`, suspend tests |
| 3 | Convert `resume_after_wake_lock`'s no-policy branch (`{paused} → resuming`) | **Change:** a resume that lost the race returns `Aborted` instead of quarantining. Flips finding 2. | `paused_suspend_serializes_resume_behind_wake_and_artifact_locks`, `auth_lookup` tests |
| 4 | Convert `create_with_id` (`{creating} → created`, `{created} → suspended`) and `try_start_from_warm_snapshot` (`{created, stopped} → suspended`, or whatever D1 decides) | **Changes:** create refuses over `stopping`; warm start refuses `started`, `paused` and transient rows with `FailedPrecondition`. Flips findings 3 and 4. | `grpc_core` tests (#271), `grpc_pool_wait_clone` tests, `start/tests.rs` |
| 5 | Convert the rest with explicit from-sets: process-snapshot rollback, template snapshot completion, auto-lifecycle Stop, the startup recovery writers, and the fixture in `application_state_write_updates_only_state_changed_at` | Findings 5 and 6 refuse; the others are unchanged | Startup recovery tests, `grpc_spaced_process_snapshots` tests |
| 6 | Re-implement `update_state` as `update_state_from(ALL)` with `GenericCleanupSupersession` in SQL. Make it `#[cfg(test)]` or delete it once no production caller remains, and move test fixtures to a test-only raw setter. | none (PR0(i) matrix) | PR0(i), `every_allowed_state_transition_tracks_the_change_time`, `cleanup_intent_cannot_be_replaced_by_suspend_or_resident_completion`, `quarantined_state_is_not_exited_by_generic_failure_writers` |
| 7 | *(optional, D3)* Move single-statement CAS writers onto `write_state`: the five eviction claims, `begin_suspend`, `rollback_suspending_to_paused`, `restore_checkpoint_state`, `recover_interrupted_pause_state`, `rollback_activation_state`, `publish_restored_*`, `publish_stopped_artifact`, `pin_suspended_artifact_for_cleanup` | none (explicit `Keep` policies) | `state_writer_refusal.rs` (all), PR0(iii), `specialized_state_writers_track_the_change_time` |
| 8 | *(optional)* Episode-bound writers: `begin_activation_with_route_evidence`, `mark_start_succeeded`, `mark_restore_succeeded`, `complete_pool_clone`, `complete_import`, `rollback_activation_episode`, `request_cleanup_episode`, `record_cleanup_episode_failure`, `quarantine_cleanup_episode_for_job`, `rearm_prepared_clone_disposal`, `apply_cleanup_state` | none | `state_writer_refusal.rs`, `cleanup_quarantine_refusal.rs`, cleanup executor tests, PR0(iii) |
| 9 | *(optional)* Quarantine writers: `mark_quarantined_inner`, `recover_pause_failed_quarantine`, `publish_orphan_quarantine_recovered_artifact`, `adopt_quarantine_cleanup_episode`, `settle_startup_resident`. `restore_adopt_orphan_or_insert_sandbox_in_transaction` stays an INSERT…ON CONFLICT and is only listed in the table. | none | Quarantine recovery tests, `generic_cleanup_writers_refuse_quarantined_and_missing_rows` |
| 10 | Cleanup: delete the dead and test-only writers in finding 8, drop F-d's branch and cleanup admission's unreachable `else`. Decide and apply D2 (Stop from non-running states) and D4 (receipt and `failure_reason` policy) as separately listed behavior changes. Update the sbx-mgr `AGENTS.md` writer rule. | listed per item | Tests from PRs 0 and 1 |

**Coverage gaps that block specific steps:**
- Before PR 2: the service-level Pause/Stop interleaving needs a barrier
  hook. `cleanup_executor.admission_barrier` is the precedent. DB-level
  sequences are enough for the writer change itself.
- Before PR 4: create vs Stop, and warm start on `started`.
- Before PR 7: whole-row accepted-shape diffs. Today acceptance is
  spot-checked per column, while refusal is checked whole-database.
- Before D2: Stop admission from each source state.

**Open PRs that would conflict or interact:**

| PR | Interaction |
|---|---|
| #270 | Edits `db/sandboxes.rs`, `db/tests.rs` and `db/tests/state_writer_refusal.rs`, and makes `database_rows` crate-visible. Land it first; PR0 reuses that helper. |
| #271 (tests only, `grpc_core/tests.rs`) | Prerequisite for PR 4 and D2 |
| #275 (sbx-mgr `AGENTS.md`) | Its rule "no generic writer compares against the caller's observed state" describes today's design. PR 1 or PR 6 must amend it. |
| #246 | Touches `service/cleanup.rs`, `service/recovery.rs`, `grpc_lifecycle/suspend_snapshot.rs` and recovery staging. Overlaps PRs 5, 8 and 10. |
| #212, #217 | `start.rs`, `auth_lookup.rs`, `snapshot_scheduler.rs`; overlap PRs 3, 4 and 7 |
| #203, #176 | `snapshot_scheduler.rs`, `grpc_lifecycle.rs`, `suspend_snapshot.rs`; overlap PR 7 |
| #201, #221 | `grpc_lifecycle.rs`, `activation_support.rs` (home of `update_state_best_effort`); minor |
| #110 (draft), #3 (conflicting) | Pre-split `db.rs` and new quarantine and activation writers; both need a rebase, and new writers should use the helper |
| #272 (api crate) | No file overlap. It applies the same pattern to the control-plane row (`found` lifecycle decides keep or write in one transaction). Shared vocabulary such as `Refused { found }` would help reviewers. |

## 6. Risks and alternatives

**Do nothing.**
- Benefits:
  - Zero churn.
  - Specialized writers are already CAS-guarded and well pinned.
  - Open PRs are unaffected.
- Costs:
  - Findings 1–6 remain.
  - Every new lifecycle writer hand-writes 20–60 lines of SQL, with side
    columns, restore-column clearing and receipt policy re-decided each
    time (F-e).
  - About 45 undocumented edges keep `state.rs` misleading.
  - Error vocabulary stays fragmented across seven return shapes: `bool`,
    `Err(QueryReturnedNoRows)`, silent `Ok(())`, `Option`, and three outcome
    enums.

**Smaller step: make `update_state` take an expected state** (PRs 0–6
without 7–9).
- Benefits:
  - Fixes every ranked race.
  - Turns D1 into an explicit, tested allowlist.
  - About 15 call sites change.
  - Low review cost, and few conflicts (mostly #212, #217, #246).
- Costs:
  - Leaves the specialized writers' SQL and side-column drift as they are.
  - A second "expected-state" API coexists with the specialized writers
    until PRs 7–9 or forever.
- **This is the recommended first commitment.**

**Full table-driven helper** (PRs 0–10).
- Benefits:
  - One place for the edge table, the defaults that the schema `CHECK`
    makes mandatory, `fail_active` on `deleting`, typed refusals with
    `found`, and event inputs.
  - The parity test makes new disagreements impossible to merge silently.
- Risks:
  - **The helper becomes a god object.** The `Guard` and `Column` enums grow
    to cover `apply_cleanup_state`'s route-identity CASE and the
    quarantine-claim fence. Mitigation: static `Raw` fragments, and keep
    INSERT…ON CONFLICT writers out.
  - **Side-column drift during moves.** Mitigation: PR0(iii) whole-row
    diffs, and explicit `Keep` policies where behavior differs today.
  - **Trigger interplay.** The `UPDATE OF state` triggers (inactivity arming)
    and the route-retention trigger depend on which columns one statement
    updates. Keep each writer's columns in the same statement as the state
    change, which `Column` allows.
  - **Statement cache pressure.** There are about 40 generated statements
    against `STATEMENT_CACHE_CAPACITY = 128`, and hot-path reads use
    `prepare_cached`. Size the cache or generate SQL statically per edge.
  - **Nested transactions.** `write_state` must never open a transaction.
    Only `write_state_standalone` may, and only in autocommit.
  - **Conflicts with roughly eight open PRs.**
  - **Review fatigue on behavior-preserving churn** in code that already has
    strong refusal tests.

**Schema trigger enforcing the edge table** (`BEFORE UPDATE OF state` with an
allowed-pair list).
- Benefits: defense in depth, and it covers future raw SQL.
- Costs:
  - Needs a migration.
  - Failures are loud `ABORT`s rather than typed refusals.
  - Cannot express guards.
  - Breaks the many test fixtures that seed states with raw `UPDATE
    sandboxes SET state` (for example `insert_with_episode`) unless they use
    a test-only bypass.

  It is worth considering after PR 6, not instead of it.

**Per-edge typestate functions** (compile-time edges, for example
`edges::pause_rollback(conn, id)`). Each is a thin wrapper over `write_state`
and gives the strongest call-site guarantee. It is compatible with the helper
and can be adopted incrementally.

## Decision summary (repeated)

**Recommendation:**
- Land the PR0 characterization tests.
- Add `update_state_from` with explicit from-sets and
  `Applied/Refused{found}/Missing`.
- Convert pause, resident resume, create and warm start first (PRs 2–4).
- Make `update_state` test-only (PR 6).
- Decide afterwards whether to move the already-guarded specialized writers
  onto the table-driven helper (PRs 7–9).

**Owner decisions:**
- **D1.** Is `SandboxState::transition` an RPC-admission machine with a
  separate persisted-edge table (recommended), or should it be widened to
  the roughly 45 implemented edges?
- **D2.** Should Stop admission honor the state machine, as Kill does?
- **D3.** Smaller step only, or the full helper?
- **D4.** Should receipt and `failure_reason` clearing be unified across
  writers?

**Ranked findings:**

| # | Risk | Finding |
|---|---|---|
| 1 | High | Pause rollback and completion overwrite a concurrent Stop's `stopped` with `started` or `paused`. Confirmed at the DB level. |
| 2 | Med-high | No-policy resident resume writes `resuming` over `stopped`, then quarantines the sandbox. Confirmed at the DB level. |
| 3 | Medium | Warm-snapshot Start writes `suspended` from unvalidated states, including `started`. |
| 4 | Medium | Create completion overwrites a concurrent Stop's `stopping` with `created` or `suspended`. The Stop is lost and the row outlives a failed create. Confirmed at the DB level. |
| 5 | Low-med | Full process-snapshot rollback overwrites a concurrent Stop's `stopping` with `suspended`. |
| 6 | Low | Template base-snapshot completion writes `paused` over `stopped`. |
| 7 | Low | Auto-lifecycle Stop writes the forbidden edge `suspended → stopped`; the race window is small. |
| 8 | Info | Eight dead or test-only writers, two unreachable branches, side-column drift (F-e), and the `Applied` outcome for a non-owned intent without a job id (F-f). |
