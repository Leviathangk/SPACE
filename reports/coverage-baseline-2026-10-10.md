# Coverage baseline for sandbox-lifecycle code (2026-10-10)

Measured before the consolidation refactors so that the next wave knows where to
add characterization tests first. This is measurement only: no production or
test code changed. Everything below comes from local runs of the packages' own
test suites at commit `15a2209d1381331be65178baffd1bb6dc96804c8`.

Companion data: [`coverage-baseline-2026-10-10.csv`](coverage-baseline-2026-10-10.csv)
(`package,file,function,lines,covered_lines,branch_pct`, one row per source
function in the files this report names, plus the whole supervisor package;
`function` is `name@line` and `file` is relative to the package root).

## Summary

| Package | Functions | 0 %-covered functions | Line coverage | Branch-outcome coverage |
|---|---|---|---|---|
| `pplx-space-sbx-mgr` | 2,278 | 495 | 33,650/52,960 (63.5%) | 3,834/6,620 (57.9%) |
| `pplx-space-node-mgr` | 1,232 | 517 | 10,478/21,236 (49.3%) | 1,303/2,888 (45.1%) |
| `pplx-space-api` | 2,717 | 644 | 36,236/55,551 (65.2%) | 2,933/5,012 (58.5%) |
| `pplx-space-node-runtime-supervisor` | 89 | 35 | 723/1,554 (46.5%) | 75/216 (34.7%) |

- **sbx-mgr `db.rs` state writers are well covered on the success path and
  often uncovered on the refusal path.** 38 functions write `sandboxes.state`.
  37 of them run in at least one test. For the 35 that report or branch on a
  compare-and-set outcome, the refusal outcome (the guarded `UPDATE` matched
  0 rows) is exercised in 17 and **never in 18**. `rollback_activation_episode`
  and its two public wrappers are never called by any test.
- **The service layer is where the lines are missing.** In the sbx-mgr suspend
  pipeline (`grpc_lifecycle.rs`), the ~600-line
  `complete_suspend_snapshot_publication`, `pause`, `pause_with_reconciliation`,
  `finalize_suspended_runtime` and `enqueue_suspend_snapshot_reupload` are never
  executed. 16 of the 22 non-test call sites of `update_state` (not counting
  its own recursive call) are never reached. These include every call site in
  `grpc_lifecycle.rs`, `create.rs`, `start.rs`, `cleanup.rs`, `auth_lookup.rs`,
  `activation_support.rs` and the snapshot paths. `create.rs` is at 7.8 % line coverage and `start.rs` at 31 %.
- **node-mgr's host-facing RPCs are mostly unexecuted.** `runtime.rs` is at 14 %,
  `launch_prepare.rs` at 17 % and `snapshots.rs` at 39 %. Most of the missing
  lines sit behind Firecracker, jailer, btrfs or runtime-supervisor calls. The
  per-sandbox mutation-claim contention path is covered at its core
  (`try_claim_sandbox_mutation` → `None` → "…in progress; retry"), but only 4 of
  15 claiming RPC call sites ever take their contention return.
- **api:** `kubernetes/store.rs` (93 %) and `execution.rs` (87 %) are solid.
  `adapter.rs` (48 %) and `sandbox/lifecycle.rs` (54 %) carry the gaps:
  `suspend_execution`, `prepare_pooled_execution`,
  `apply_adopted_per_sandbox_setup` and `untracked_action` are all at 0 %.
- Suites are fast and deterministic. Uninstrumented test execution takes
  1.3 s / 3.2 s / 7.6 s / 13.9 s (supervisor / node-mgr / sbx-mgr / api). No test
  flaked in 5 full runs per package. Only api needs external services: 707 of
  its 1,656 tests use Postgres or Redis.

## Method

| Step | Detail |
|---|---|
| Tooling | `cargo-llvm-cov 0.9.1`, `cargo-nextest 0.9.133` (the CI pin), pinned toolchain `1.98.0`, `protoc 35.0` (the CI image pin). |
| Instrumentation | `cargo llvm-cov show-env --branch` (`-C instrument-coverage -Z coverage-options=branch`). Branch coverage is unstable, so the pinned stable compiler ran with `RUSTC_BOOTSTRAP=1`. |
| Test command | Per package: `cargo nextest run --locked --all-targets --all-features -p <pkg> --run-ignored all --retries 1 --no-fail-fast`, using the repository nextest config. That config's run-scoped Postgres and Redis setup scripts started real containers, and `SPACE_CI_REQUIRE_TEST_DEPS=1` was set. The dependency-skip log stayed empty (0 `SPACE-TEST-DEP-SKIP` records) in every run. |
| Reporting | `cargo llvm-cov report --branch --json`. LLVM 22.1.8 `llvm-cov export` (rustc 1.98.0's llvm-tools) segfaults in `CoverageMapping::getInstantiationGroups` on these test binaries, even single-threaded and on the smallest package. The export therefore used the nightly llvm-tools `llvm-cov` (LLVM 23.1.3) to read the 1.98.0-built profiles; profile merging stayed on 1.98.0's `llvm-profdata`. |
| Per-function numbers | Functions are source `fn` items. Closures and `async` bodies roll up into their enclosing `fn`, and generic instantiations are merged. "Lines" are LLVM-mapped lines (LLVM's own line-coverage rule; multi-line SQL literals count as lines). "Branch outcomes" counts the true/false outcomes of conditions rustc instruments (`if`, `while`, `let … else`, `&&`/`||` operands, `if let`). `?` and plain `match` arms are not counted as branches. Test bodies, `tests.rs`/`*_tests.rs` files and `#[cfg(test)]` items are excluded. |
| Per-test attribution | A target runner gave every nextest test process (and any child it re-executes, such as `enter_test_process`) its own profile file. Each test's profile was exported separately, and the union of the 3,541 per-test results equals the aggregate exactly (checked on three files). Test names in this report come from that attribution. |
| Refusal-branch probe | `Ok(changed == 1)`-style returns have no branch region, so a 0-row refusal is invisible to line coverage. A measurement-only build rewrote those 45 one-line returns in `db.rs` and `db/transfer.rs` to `Ok(if changed == 1 { black_box(true) } else { black_box(false) })` on the same line, in a separate target directory. It reran the sbx-mgr suite (same 1,458 pass / 1 root-only failure) and was reverted; nothing from it is committed. "Refusal" columns below come from that probe. |
| "Adjusted uncovered" | Uncovered lines minus lines that are only punctuation or `?` (error-propagation tails like `)?;`). The ranking uses this to avoid rewarding untested `?` arms. |

Limits: this is unit/integration coverage only. Kind and typed smoke suites
exercise more of the host-facing paths and were not measured. Doctests were not
measured (`--doctests` instrumentation is unstable).

## Q6 — suite wall time, external services, flakes

| Package | Tests | Test execution, instrumented | Test execution, uninstrumented | Cold build of all four packages' tests | External services |
|---|---|---|---|---|---|
| `pplx-space-node-runtime-supervisor` | 23 | 1.4 s | 1.3 s | 223 s instrumented / 263 s plain (32 cores, shared by the four packages) | none |
| `pplx-space-node-mgr` | 402 (9 ignored) | 3.8 s | 3.2 s | ″ | none |
| `pplx-space-sbx-mgr` | 1,459 (1 ignored) | 12.4 s | 7.6 s | ″ | none (SQLite only) |
| `pplx-space-api` | 1,657 (1 ignored) | 16.4 s | 13.9 s | ″ | Postgres (~686 tests), Redis (21 tests: `client_concurrency`, `cluster_admission`) |

- "Test execution" is nextest's `Summary` time. Running `cargo nextest run -p <pkg>`
  alone after a combined build re-links per-package feature sets, so the
  first per-package invocation also spends 25–180 s compiling.
- External services: I reran every suite with no setup scripts, Docker
  unreachable and `SPACE_CI_REQUIRE_TEST_DEPS=1`. supervisor, node-mgr and
  sbx-mgr passed in full. api failed exactly 707 tests: 184 under
  `controllers::substrate::kubernetes`, 53 `controllers::substrate::direct`,
  36 `routes::admin::node_store`, 36 `controllers::sandbox::queries`,
  33 `controllers::sandbox::lifecycle`, and the rest in admin, public, E2B and
  auth routes. The Postgres container starts in about 1.5 s.
- Flakes: none. Each package ran 5 times (1 attributed run, 3 instrumented
  reruns, 1 uninstrumented run) with `--retries 1`, and sbx-mgr ran 3 more times
  during the probe work. No test needed a retry, and nextest reported no SLOW,
  TIMEOUT or LEAK status.
- Deterministic failures, all ignored tests that need a disposable root
  container (CI runs ignored tests as root):
  `node-mgr clone_tree::tests::private_clone_blocks_foreign_uid_grafts_and_shared_clone_is_held`,
  `node-mgr loop_devices::tests::ensure_capacity_creates_attachable_devices_past_max_loop`,
  `sbx-mgr transfer_directory::tests::distinct_service_uids_can_exchange_downloads`.
  Classification: ENVIRONMENTAL (non-root box). Their files are outside this
  report's scope. The other 7 ignored node-mgr tests passed without root, and
  their btrfs/device code is also outside scope.

## Q1 — sbx-mgr `db.rs`: every writer of `sandboxes.state`

The writers are the 38 `UPDATE sandboxes … SET state` statements (in 33 functions;
`claim_suspended_sandbox_for_eviction` has 4 SQL variants), the two
`INSERT INTO sandboxes` paths, and `db/transfer.rs::complete_import`.

How to read the columns:
- *Success path*: the guarded write matched its row. The count is the number of
  executions that did.
- *Refusal*: the primary compare-and-set (CAS) on the state `UPDATE` matched
  0 rows. The value comes from in-function branches or from the probe described
  under Method.
- *Pre-check guards hit*: early refusal returns before the `UPDATE` (stale
  witness, quarantine block, unsupported state, wrong episode), as hit/total.
- *Secondary*: CAS checks on companion rows written in the same transaction.
- Example tests are the two most focused tests that execute the function (the
  fewest total lines executed). The CSV and the reproduction steps give the
  full sets.

| # | Function (file:line) | Lines cov | Branch outcomes | # tests | Example tests | Success path | Refusal (CAS 0 rows) | Pre-check guards hit | Tests hitting refusal | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `insert_sandbox` `db.rs:1633` | 49/50 | 0/0 | 649 | `db::inventory_read::tests::state_metadata_batches_share_the_callers_budget`, `db::tests::execution_preflight_reports_legacy_rows_without_changing_them` | yes | n/a | – | – | INSERT; no CAS. Refusal = constraint error only. |
| 2 | `restore_adopt_orphan_or_insert_sandbox_in_transaction` `db.rs:1970` | 146/148 | 3/4 | 10 | `db::tests::restore_adopt_insert_binds_volume_encryption`, `db::tests::restore_adoption_clears_obsolete_auto_lifecycle_delete_block` | yes (11) | yes (1) | 0/1 | `service::recovery::tests::cross_node_restore_rejects_existing_non_orphan_row_without_overwrite` | INSERT … ON CONFLICT adopt; `rows_affected` refusal hit. |
| 3 | `update_state` `db.rs:2153` | 78/79 | 18/20 | 66 | `db::tests::application_state_write_updates_only_state_changed_at`, `db::tests::generic_lifecycle_writers_reject_failed_targets` | yes (146) | yes (3) | 3/3 | `service::tests::service_tests::untracked_cleanup_admission_preserves_the_node_escape_hatch`, `service::tests::service_tests::unreadable_ownership_blocks_an_already_admitted_cleanup_fallback` | Refusals: Quarantined/Failed target (L2159), quarantine block (L2177), cleanup-owned (L2211), missing row (L2254). |
| 4 | `settle_startup_resident` `db.rs:2300` | 88/90 | 7/8 | 8 | `db::tests::startup_missing_runtime_identity_quarantines_without_cleanup`, `db::tests::startup_resident_settlement_keeps_only_current_receipts_atomically` | yes | n/a | 1/2 | – | Refusal is the stale-witness pre-check (L2336); no CAS on the UPDATE itself. |
| 5 | `restore_checkpoint_state` `db.rs:2407` | 18/20 | 1/2 | 4 | `snapshot_scheduler::tests::completed_checkpoint_remains_publishable_when_state_restore_is_superseded`, `snapshot_scheduler::tests::checkpoint_failure_restores_the_previous_stable_state` | yes (6) | yes (2) | 0/1 | `snapshot_scheduler::tests::completed_checkpoint_remains_publishable_when_state_restore_is_superseded`, `snapshot_scheduler::tests::checkpoint_failure_restores_the_previous_stable_state` |  |
| 6 | `recover_interrupted_pause_state` `db.rs:2471` | 17/18 | 0/0 | 1 | `service::tests::service_tests::startup_recovery_heals_definitive_pause_and_quarantines_ambiguity` | yes (2) | **no** | – | – |  |
| 7 | `begin_activation_with_route_evidence` `db.rs:2588` | 120/129 | 12/12 | 39 | `db::tests::cleanup_episode_recovery_fence_requires_artifact_and_generation_together`, `db::tests::activation_policy_failure_rolls_back_the_execution_claim` | yes (148) | yes (3) | 1/2 | `db::tests::activation_entry_rollbacks_and_quarantines_do_not_overwrite_newer_state`, `auto_lifecycle_delete_receipts::tests::beginning_an_execution_cannot_supersede_a_current_pending_receipt` | Also refuses on existing cleanup episode (L2613). |
| 8 | `mark_start_succeeded` `db.rs:3055` | 44/45 | 8/8 | 7 | `db::tests::start_arms_access_idle_only_after_guest_readiness`, `db::tests::legacy_equal_route_and_operation_receipt_survives_restart_without_split_field` | yes (17) | yes (1) | – | `db::tests::mark_start_succeeded_requires_starting_state` |  |
| 9 | `rearm_prepared_clone_disposal` `db.rs:3120` | 21/23 | 1/2 | 2 | `service::grpc_pool_wait_clone::tests::pool_clone_restart_preserves_disposal_ownership`, `service::grpc_pool_wait_clone::tests::pool_clone_cleanup_never_falls_back_and_holds_unsafe_or_exhausted_disposal` | yes (6) | **no** | – | – |  |
| 10 | `complete_pool_clone` `db.rs:3164` | 33/34 | 2/2 | 3 | `service::grpc_pool_wait_clone::tests::pool_clone_and_adoption_keep_donor_ownership_exclusive`, `service::grpc_pool_wait_clone::tests::pool_clone_restart_preserves_disposal_ownership` | yes (2) | yes (23) | – | `service::grpc_pool_wait_clone::tests::pool_clone_restart_preserves_disposal_ownership`, `service::grpc_pool_wait_clone::tests::pool_clone_publication_and_disposal_are_exclusive` |  |
| 11 | `mark_restore_succeeded` `db.rs:3239` | 62/63 | 13/14 | 12 | `db::tests::mark_restore_succeeded_requires_restoring_state`, `db::tests::specialized_state_writers_track_the_change_time` | yes (13) | yes (3) | 0/1 | `db::tests::mark_restore_succeeded_requires_restoring_state`, `service::activation_support::tests::restore_final_state_requires_a_restoring_row` |  |
| 12 | `begin_suspend` `db.rs:3330` | 24/25 | 0/0 | 11 | `db::tests::cleanup_intent_cannot_be_replaced_by_suspend_or_resident_completion`, `db::tests::publish_restored_suspended_artifact_dedupes_prior_suspend_row` | yes (16) | yes (13) | – | `db::tests::cleanup_intent_cannot_be_replaced_by_suspend_or_resident_completion` |  |
| 13 | `begin_teardown_suspend` `db.rs:3356` | 35/37 | 1/2 | 22 | `db::tests::teardown_suspend_journal_persists_the_local_execution_lease`, `db::tests::restaging_the_same_teardown_artifact_preserves_its_durability_marker` | yes (29) | **no** | – | – |  |
| 14 | `rollback_suspending_to_paused` `db.rs:3586` | 22/23 | 0/0 | 4 | `service::tests::service_tests::suspend_snapshot_publication_failure_preserves_restored_base_for_live_retry`, `service::tests::service_tests::startup_recovery_rolls_live_resuspend_back_to_its_predecessor` | yes (4) | **no** | – | – |  |
| 15 | `rollback_activation_episode` `db.rs:3686` | 0/55 | 0/8 | 0 | – | **no** | **no** | 0/3 | – | Never called by any test (see dead-code note). |
| 16 | `rollback_activation_state` `db.rs:3746` | 24/25 | 0/0 | 7 | `db::tests::generic_lifecycle_writers_reject_failed_targets`, `db::tests::start_arms_access_idle_only_after_guest_readiness` | yes (12) | yes (3) | 1/1 | `db::tests::activation_entry_rollbacks_and_quarantines_do_not_overwrite_newer_state`, `service::auth_lookup::tests::connection_failure_rolls_back_only_the_admitted_resume` |  |
| 17 | `pin_suspended_artifact_for_cleanup` `db.rs:3856` | 36/38 | 1/2 | 6 | `service::tests::service_tests::startup_recovery_adopts_resuspend_successor_instead_of_predecessor`, `service::tests::service_tests::suspend_publish_failure_pins_complete_canonical_when_runtime_is_gone` | yes (6) | **no** | – | – |  |
| 18 | `publish_restored_suspended_artifact` `db.rs:4007` | 45/48 | 1/2 | 2 | `db::tests::publish_restored_suspended_artifact_dedupes_prior_suspend_row`, `service::recovery::tests::cross_node_restore_row_lifecycle_stays_restoring_until_rootfs_stage_succeeds` | yes (2) | **no** | – | – |  |
| 19 | `publish_restored_suspended_without_artifact` `db.rs:4069` | 24/26 | 1/2 | 3 | `db::tests::retryable_restore_scope_handoff_rejects_a_after_b_and_admits_c`, `service::recovery_cross_node::tests::exchanged_restore_cleanup_failure_retains_scope_until_retry` | yes (3) | **no** | – | – |  |
| 20 | `publish_restored_disk_as_new` `db.rs:4096` | 23/25 | 1/2 | 2 | `db::tests::restored_disk_as_new_becomes_a_stopped_sandbox_without_donor_runtime_identity`, `service::recovery_cross_node::tests::interrupted_restore_as_new_publishes_metadata_and_finalizes_as_stopped` | yes (2) | **no** | – | – |  |
| 21 | `publish_stopped_artifact` `db.rs:4214` | 66/69 | 1/2 | 3 | `db::tests::deleting_named_snapshot_identity_does_not_delete_current_instance_artifact`, `db::tests::delete_sandbox_removes_local_artifacts` | yes (3) | **no** | – | – |  |
| 22 | `recover_pause_failed_quarantine` `db.rs:4746` | 88/102 | 5/8 | 4 | `db::tests::pause_failed_recovery_requires_exact_quarantine_fence`, `db::tests::pause_failed_quarantine_recovers_to_definitive_runtime_state` | yes (5) | **no** | 1/1 | – | Secondary: `recorded`≠1 refusal **never**; `cleared`≠1 refusal **never**. |
| 23 | `publish_orphan_quarantine_recovered_artifact` `db.rs:5671` | 67/70 | 5/6 | 7 | `db::tests::publishing_a_recovered_orphan_leaves_no_quarantine_row`, `db::tests::a_publishing_orphan_restore_never_becomes_an_adoption_candidate` | yes (14) | yes (10) | 0/1 | `db::tests::every_claim_fenced_statement_pins_every_conjunct`, `db::tests::quarantine_adoption_completes_with_the_authority_receipt_id` | Secondary: `retired`≠1 refusal **never**. |
| 24 | `adopt_quarantine_cleanup_episode` `db.rs:5835` | 86/89 | 3/4 | 6 | `db::tests::every_claim_fenced_statement_pins_every_conjunct`, `db::tests::quarantine_adoption_completes_with_the_authority_receipt_id` | yes (30) | yes (45) | – | `db::tests::every_claim_fenced_statement_pins_every_conjunct`, `service::recovery::tests::two_actors_in_two_windows_still_leave_every_fenced_write_missing` | Secondary: `released`≠1 refusal **never**. |
| 25 | `mark_quarantined_inner` `db.rs:5989` | 165/172 | 20/26 | 107 | `db::tests::mark_quarantined_reports_unknown_current_state`, `db::tests::pause_failed_recovery_requires_exact_quarantine_fence` | yes (597) | **no** | 1/1 | – |  |
| 26 | `request_cleanup_episode` `db.rs:6422` | 79/87 | 8/12 | 22 | `db::tests::repeated_cleanup_request_preserves_failure_and_missing_artifact_evidence`, `db::tests::quarantine_cleanup_records_the_current_failure` | yes (118) | **no** | 1/2 | – | Secondary: `episode_changed`≠1 refusal **never**. |
| 27 | `record_cleanup_episode_failure` `db.rs:6515` | 49/52 | 5/8 | 5 | `db::tests::repeated_cleanup_request_preserves_failure_and_missing_artifact_evidence`, `db::tests::quarantine_cleanup_records_the_current_failure` | yes (41) | **no** | – | – |  |
| 28 | `apply_cleanup_state` `db.rs:6728` | 114/115 | 2/2 | 44 | `db::tests::cleanup_recovery_owns_failure_clearance_not_stale_resident_completion`, `db::tests::publish_restored_suspended_artifact_dedupes_prior_suspend_row` | yes (141) | **no** | – | – | Refusal (0 rows) never observed. |
| 29 | `quarantine_cleanup_episode_for_job` `db.rs:7408` | 84/97 | 9/16 | 6 | `db::tests::a_failed_restore_naming_no_artifact_is_correctly_left_for_an_operator`, `db::tests::a_failed_restore_recovers_only_if_the_two_artifact_stores_diverge` | yes (10) | **no** | 0/5 | – |  |
| 30 | `mark_cleanup_required` `db.rs:7514` | 34/37 | 1/2 | 17 | `db::tests::specialized_state_writers_track_the_change_time`, `db::tests::startup_recovery_uses_runtime_fields_to_choose_cleanup_destination` | yes | n/a | – | – | UPDATE result is discarded (returns `()`); only the quarantine-block guard is a refusal and it is uncovered. |
| 31 | `mark_cleanup_required_if_no_intent_inner` `db.rs:7587` | 52/55 | 3/4 | 6 | `db::tests::mark_cleanup_required_if_no_intent_preserves_foreign_intent`, `service::tests::service_tests::checkpoint_retry_preserves_foreign_cleanup_intent` | yes (5) | yes (2) | 0/1 | `db::tests::mark_cleanup_required_if_no_intent_preserves_foreign_intent`, `service::tests::service_tests::checkpoint_retry_preserves_foreign_cleanup_intent` |  |
| 32 | `mark_cleanup_failed` `db.rs:7670` | 42/45 | 3/6 | 8 | `db::tests::cleanup_recovery_owns_failure_clearance_not_stale_resident_completion`, `service::tests::service_tests::requested_and_startup_recovery_keep_distinct_authority` | yes (8) | **no** | 0/1 | – |  |
| 33 | `claim_suspended_sandbox_for_eviction` `db.rs:9339` | 39/52 | 2/2 | 7 | `sandbox_eviction::tests::claim_cas_rejects_unpinned_claim_when_pin_appeared`, `sandbox_eviction::tests::claim_cas_rejects_non_suspended_rows` | yes (6) | yes (4) | – | `sandbox_eviction::tests::claim_cas_rejects_unpinned_claim_when_pin_appeared`, `sandbox_eviction::tests::claim_cas_rejects_non_suspended_rows` | 4 SQL variants by (pin, updated_at): the (Some pin, Some updated_at) arm at L9347 is never executed; the other three are. |
| 34 | `claim_failed_restoring_sandbox_for_eviction` `db.rs:9420` | 25/26 | 2/2 | 2 | `service::tests::service_tests::failed_restoring_eviction_claim_requires_the_marker_and_the_row_version`, `service::tests::service_tests::evict_cluster_deleted_sandbox_reclaims_retained_restoring_and_quarantined_rows` | yes (2) | yes (4) | – | `service::tests::service_tests::failed_restoring_eviction_claim_requires_the_marker_and_the_row_version` |  |
| 35 | `claim_quarantined_sandbox_for_eviction` `db.rs:9457` | 17/18 | 2/2 | 2 | `service::tests::service_tests::quarantined_eviction_claim_rejects_stale_version_and_recovered_state`, `service::tests::service_tests::evict_cluster_deleted_sandbox_reclaims_retained_restoring_and_quarantined_rows` | yes (2) | yes (2) | – | `service::tests::service_tests::quarantined_eviction_claim_rejects_stale_version_and_recovered_state` |  |
| 36 | `claim_stopped_sandbox_for_eviction` `db.rs:9479` | 14/16 | 1/2 | 1 | `service::tests::service_tests::evict_cluster_deleted_sandbox_reclaims_retained_restoring_and_quarantined_rows` | yes (1) | **no** | – | – |  |
| 37 | `claim_teardown_suspend_failed_sandbox_for_eviction` `db.rs:9525` | 25/26 | 2/2 | 4 | `service::tests::service_tests::terminal_teardown_suspend_marker_is_scoped_and_leaves_the_row_version_alone`, `service::tests::service_tests::terminal_teardown_suspend_claim_asserts_identity_not_the_row_version` | yes (5) | yes (4) | – | `service::tests::service_tests::terminal_teardown_suspend_claim_asserts_identity_not_the_row_version` |  |
| 38 | `complete_import` `db/transfer.rs:57` | 26/27 | 4/4 | 1 | `service::grpc_sandbox_transfer::tests::import_reservation_preserves_existing_files_and_fences_completion` | yes (1) | yes (2) | – | `service::grpc_sandbox_transfer::tests::import_reservation_preserves_existing_files_and_fences_completion` |  |

### Q1 findings

- **Refusal never exercised (18 functions).** `recover_interrupted_pause_state`,
  `rearm_prepared_clone_disposal`, `begin_teardown_suspend`,
  `rollback_suspending_to_paused`, `pin_suspended_artifact_for_cleanup`,
  `publish_restored_suspended_artifact`,
  `publish_restored_suspended_without_artifact`,
  `publish_restored_disk_as_new`, `publish_stopped_artifact`,
  `recover_pause_failed_quarantine` (and its history/retirement invariants),
  `mark_quarantined_inner` (`changed == 0` warning path),
  `request_cleanup_episode`, `record_cleanup_episode_failure`,
  `apply_cleanup_state`, `quarantine_cleanup_episode_for_job` (also 0/5
  pre-checks), `mark_cleanup_failed`, `claim_stopped_sandbox_for_eviction`, and
  `rollback_activation_episode` (never executed at all). Every one of these is
  DB-only and can be pinned with `db::test_conn()` in a sibling test module.
- **Never executed:** `rollback_activation_episode` (`db.rs:3686`) and its
  wrappers `rollback_restoring_episode` (`db.rs:3674`, called from
  `service/create.rs:805`) and `rollback_starting_episode` (`db.rs:3683`, called
  from `service/start.rs:1868`). This is live production code with no test.
- **Uncovered quarantine guard on generic writers.** In `mark_cleanup_required`,
  `mark_cleanup_required_if_no_intent_inner` and `mark_cleanup_failed`, no test
  reaches the `generic_writer_transition_blocked` refusal. In `update_state` the
  same guard (L2177) is covered.
- `claim_suspended_sandbox_for_eviction` never executes its
  `(Some(pin), Some(updated_at))` SQL variant (`db.rs:9347`).

### `update_state` call sites (22 outside tests, plus its own recursive call at `db.rs:2163`)

| Call site | Enclosing function | Executions | Tests reaching it | Example test |
|---|---|---|---|---|
| `db.rs:2163` | `update_state` | 22 | 6 | `db::tests::every_allowed_state_transition_tracks_the_change_time` |
| `db.rs:2446` | `begin_resident_resume_with_policy` | 4 | 1 | `db::tests::resident_policy_resume_is_generation_fenced_and_arms_only_after_thaw` |
| `db.rs:2539` | `update_state_superseding_activation_in_transaction` | 48 | 25 | `db::tests::teardown_suspend_terminal_transitions_close_jobs_atomically` |
| `db.rs:9845` | `recover_interrupted_startup_states` | 1 | 1 | `db::tests::startup_recovery_reconciles_interrupted_states_without_runtime_fields` |
| `service/activation_support.rs:1811` | `update_state_best_effort` | **0** | 0 | – |
| `service/auth_lookup.rs:812` | `resume_after_wake_lock` | **0** | 0 | – |
| `service/cleanup.rs:1538` | `admit_lifecycle_cleanup_fenced` | **0** | 0 | – |
| `service/create.rs:416` | `create_with_id` | **0** | 0 | – |
| `service/create.rs:528` | `create_with_id` | **0** | 0 | – |
| `service/grpc_lifecycle.rs:1620` | `pause` | **0** | 0 | – |
| `service/grpc_lifecycle.rs:1634` | `pause` | **0** | 0 | – |
| `service/grpc_lifecycle.rs:1677` | `pause` | **0** | 0 | – |
| `service/grpc_lifecycle.rs:3414` | `prepare_suspend_snapshot_publication_after_locks` | **0** | 0 | – |
| `service/grpc_lifecycle.rs:3426` | `prepare_suspend_snapshot_publication_after_locks` | **0** | 0 | – |
| `service/grpc_lifecycle.rs:3486` | `prepare_suspend_snapshot_publication_after_locks` | **0** | 0 | – |
| `service/grpc_spaced_process_snapshots.rs:1694` | `rollback` | **0** | 0 | – |
| `service/grpc_state_snapshot.rs:1784` | `create_template_base_snapshot` | **0** | 0 | – |
| `service/grpc_state_snapshot.rs:1901` | `create_template_base_snapshot` | **0** | 0 | – |
| `service/reconcile.rs:494` | `run_due_auto_lifecycle` | 14 | 2 | `service::reconcile::tests::auto_lifecycle_scheduler_preserves_storage_keys_and_drains_admitted_work` |
| `service/recovery.rs:944` | `recover_interrupted_create` | 1 | 1 | `service::tests::service_tests::startup_recovery_finishes_interrupted_create_by_creating_cache_root` |
| `service/recovery.rs:991` | `recover_unlaunched_start` | 2 | 2 | `service::tests::service_tests::startup_recovery_rewinds_unlaunched_start_with_rootfs_to_stopped` |
| `service/start.rs:1426` | `try_start_from_warm_snapshot` | **0** | 0 | – |
| `snapshot_scheduler.rs:831` | `do_checkpoint` | **0** | 0 | – |

## Q2 — `sandbox_cleanup_episodes`, `sandbox_quarantine_state`, `local_artifacts`, `sandbox_restore_transfer_scopes`, `claim_*_for_eviction`

A function appears under every table it writes. Writers that use no CAS (plain
`INSERT`/`DELETE`, or a discarded result) show `n/a` for refusal. The
`claim_*_for_eviction` family is rows 33–37 of the Q1 table: every member's
success path is covered, and the refusal path is covered for every member
except `claim_stopped_sandbox_for_eviction`.

#### `sandbox_cleanup_episodes` — 20 writers, 13 read-only functions

| # | Function (file:line) | Lines cov | Branch outcomes | # tests | Example tests | Success path | Refusal (CAS 0 rows) | Pre-check guards hit | Tests hitting refusal | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `insert_cleanup_episode` `db.rs:1792` | 28/30 | 0/0 | 48 | `db::tests::cleanup_episode_recovery_fence_requires_artifact_and_generation_together`, `db::tests::legacy_equal_route_and_operation_receipt_survives_restart_without_split_field` | yes | n/a | – | – |  |
| 2 | `update_state_superseding_activation_in_transaction` `db.rs:2508` | 29/31 | 7/8 | 26 | `db::tests::teardown_suspend_terminal_transitions_close_jobs_atomically`, `db::tests::quarantine_adoption_completes_with_the_authority_receipt_id` | yes | n/a | 1/1 | – |  |
| 3 | `begin_activation_with_route_evidence` `db.rs:2588` | 120/129 | 12/12 | 39 | `db::tests::cleanup_episode_recovery_fence_requires_artifact_and_generation_together`, `db::tests::activation_policy_failure_rolls_back_the_execution_claim` | yes (148) | yes (3) | 1/2 | `db::tests::activation_entry_rollbacks_and_quarantines_do_not_overwrite_newer_state`, `auto_lifecycle_delete_receipts::tests::beginning_an_execution_cannot_supersede_a_current_pending_receipt` | Also refuses on existing cleanup episode (L2613). |
| 4 | `bind_activation_operation` `db.rs:2881` | 26/26 | 8/10 | 2 | `db::tests::split_operation_receipts_survive_restart_and_preserve_route_ownership`, `service::tests::service_tests::split_operation_wake_join_restart_receipt_proof` | yes | n/a | 2/2 | – |  |
| 5 | `mark_start_succeeded` `db.rs:3055` | 44/45 | 8/8 | 7 | `db::tests::start_arms_access_idle_only_after_guest_readiness`, `db::tests::legacy_equal_route_and_operation_receipt_survives_restart_without_split_field` | yes (17) | yes (1) | – | `db::tests::mark_start_succeeded_requires_starting_state` |  |
| 6 | `charge_prepared_clone_disposal_attempt` `db.rs:3103` | 13/14 | 0/0 | 1 | `service::grpc_pool_wait_clone::tests::pool_clone_cleanup_never_falls_back_and_holds_unsafe_or_exhausted_disposal` | yes (19) | yes (1) | – | `service::grpc_pool_wait_clone::tests::pool_clone_cleanup_never_falls_back_and_holds_unsafe_or_exhausted_disposal` |  |
| 7 | `rearm_prepared_clone_disposal` `db.rs:3120` | 21/23 | 1/2 | 2 | `service::grpc_pool_wait_clone::tests::pool_clone_restart_preserves_disposal_ownership`, `service::grpc_pool_wait_clone::tests::pool_clone_cleanup_never_falls_back_and_holds_unsafe_or_exhausted_disposal` | yes (6) | **no** | – | – |  |
| 8 | `bind_prepared_clone_root` `db.rs:3146` | 14/15 | 0/0 | 1 | `service::grpc_pool_wait_clone::tests::pool_clone_restart_preserves_disposal_ownership` | yes (2) | **no** | – | – |  |
| 9 | `complete_pool_clone` `db.rs:3164` | 33/34 | 2/2 | 3 | `service::grpc_pool_wait_clone::tests::pool_clone_and_adoption_keep_donor_ownership_exclusive`, `service::grpc_pool_wait_clone::tests::pool_clone_restart_preserves_disposal_ownership` | yes (2) | yes (23) | – | `service::grpc_pool_wait_clone::tests::pool_clone_restart_preserves_disposal_ownership`, `service::grpc_pool_wait_clone::tests::pool_clone_publication_and_disposal_are_exclusive` |  |
| 10 | `complete_create` `db.rs:3199` | 37/39 | 3/4 | 6 | `db::tests::node_execution_cleanup_acknowledgements_are_retryable_and_fenced`, `service::tests::service_tests::incomplete_kubernetes_identity_does_not_enter_the_claim_path` | yes (6) | **no** | – | – |  |
| 11 | `mark_restore_succeeded` `db.rs:3239` | 62/63 | 13/14 | 12 | `db::tests::mark_restore_succeeded_requires_restoring_state`, `db::tests::specialized_state_writers_track_the_change_time` | yes (13) | yes (3) | 0/1 | `db::tests::mark_restore_succeeded_requires_restoring_state`, `service::activation_support::tests::restore_final_state_requires_a_restoring_row` |  |
| 12 | `rollback_activation_episode` `db.rs:3686` | 0/55 | 0/8 | 0 | – | **no** | **no** | 0/3 | – | Never called by any test (see dead-code note). |
| 13 | `delete_sandbox_child_rows` `db.rs:3901` | 23/29 | 0/0 | 36 | `db::tests::delete_sandbox_removes_local_artifact_orphans`, `db::tests::delete_sandbox_removes_network_policy_reconcile_rows` | yes | n/a | – | – |  |
| 14 | `adopt_quarantine_cleanup_episode` `db.rs:5835` | 86/89 | 3/4 | 6 | `db::tests::every_claim_fenced_statement_pins_every_conjunct`, `db::tests::quarantine_adoption_completes_with_the_authority_receipt_id` | yes (30) | yes (45) | – | `db::tests::every_claim_fenced_statement_pins_every_conjunct`, `service::recovery::tests::two_actors_in_two_windows_still_leave_every_fenced_write_missing` | Secondary: `released`≠1 refusal **never**. |
| 15 | `retire_stale_armed_cleanup_episode` `db.rs:6377` | 13/42 | 3/12 | 1 | `service::grpc_pool_wait_clone::tests::pool_clone_restart_preserves_disposal_ownership` | **no** | **no** | 1/2 | – |  |
| 16 | `request_cleanup_episode` `db.rs:6422` | 79/87 | 8/12 | 22 | `db::tests::repeated_cleanup_request_preserves_failure_and_missing_artifact_evidence`, `db::tests::quarantine_cleanup_records_the_current_failure` | yes (118) | **no** | 1/2 | – | Secondary: `episode_changed`≠1 refusal **never**. |
| 17 | `record_cleanup_episode_failure` `db.rs:6515` | 49/52 | 5/8 | 5 | `db::tests::repeated_cleanup_request_preserves_failure_and_missing_artifact_evidence`, `db::tests::quarantine_cleanup_records_the_current_failure` | yes (41) | **no** | – | – |  |
| 18 | `complete_cleanup_episode_for_job` `db.rs:6954` | 73/77 | 17/20 | 19 | `db::tests::repeated_cleanup_request_preserves_failure_and_missing_artifact_evidence`, `db::tests::quarantine_cleanup_records_the_current_failure` | yes | n/a | 2/4 | – |  |
| 19 | `quarantine_cleanup_episode_for_job` `db.rs:7408` | 84/97 | 9/16 | 6 | `db::tests::a_failed_restore_naming_no_artifact_is_correctly_left_for_an_operator`, `db::tests::a_failed_restore_recovers_only_if_the_two_artifact_stores_diverge` | yes (10) | **no** | 0/5 | – |  |
| 20 | `complete_import` `db/transfer.rs:57` | 26/27 | 4/4 | 1 | `service::grpc_sandbox_transfer::tests::import_reservation_preserves_existing_files_and_fences_completion` | yes (1) | yes (2) | – | `service::grpc_sandbox_transfer::tests::import_reservation_preserves_existing_files_and_fences_completion` |  |

Read-only functions (lines covered/total): `cleanup_episode` 78/134, `quarantine_recovery_observations` 52/59, `cleanup_recovery_page` 66/70, `read_batch` 65/68, `prune_batch` 68/71, `sandbox_guards` 23/24, `update_runtime_fields_with_activation_owner` 121/124, `claim_quarantine_recovery_candidate` 78/79, `has_cleanup_episode` 6/6, `cleanup_episode_current` 20/20, `set_cleanup_intent` 19/19, `completed_activation_rollback_covers` 25/25, `teardown_suspend_retry_admissible` 10/10.


#### `sandbox_quarantine_state` — 11 writers, 14 read-only functions

| # | Function (file:line) | Lines cov | Branch outcomes | # tests | Example tests | Success path | Refusal (CAS 0 rows) | Pre-check guards hit | Tests hitting refusal | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `delete_sandbox_child_rows` `db.rs:3901` | 23/29 | 0/0 | 36 | `db::tests::delete_sandbox_removes_local_artifact_orphans`, `db::tests::delete_sandbox_removes_network_policy_reconcile_rows` | yes | n/a | – | – |  |
| 2 | `insert_operator_quarantine_episode` `db.rs:4453` | 17/17 | 0/0 | 5 | `db::tests::quarantine_cleanup_records_the_current_failure`, `service::tests::service_tests::cleanup_completion_and_failure_apply_recovery_intent` | yes | n/a | – | – |  |
| 3 | `insert_new_quarantine_episode` `db.rs:4471` | 74/75 | 0/0 | 132 | `db::tests::pause_failed_recovery_requires_exact_quarantine_fence`, `db::tests::quarantine_recovery_new_episode_survives_a_lease_without_an_owner` | yes | n/a | – | – |  |
| 4 | `recover_pause_failed_quarantine` `db.rs:4746` | 88/102 | 5/8 | 4 | `db::tests::pause_failed_recovery_requires_exact_quarantine_fence`, `db::tests::pause_failed_quarantine_recovers_to_definitive_runtime_state` | yes (5) | **no** | 1/1 | – | Secondary: `recorded`≠1 refusal **never**; `cleared`≠1 refusal **never**. |
| 5 | `claim_quarantine_recovery_candidate` `db.rs:5388` | 78/79 | 7/8 | 60 | `db::tests::a_claim_reports_the_count_it_wrote_not_the_one_it_read`, `db::tests::quarantine_recovery_new_episode_resets_a_dead_claim` | yes | n/a | 3/3 | – |  |
| 6 | `renew_quarantine_recovery_claim` `db.rs:5482` | 31/32 | 0/0 | 4 | `db::timestamps::tests::quarantine_deadline_claims_and_live_fences_reject_sqlite_numeric_prefixes`, `db::timestamps::tests::legacy_iso_deadlines_remain_claimable_and_raw_lease_evidence_is_cas_fenced` | yes (3) | yes (17) | – | `db::timestamps::tests::quarantine_deadline_claims_and_live_fences_reject_sqlite_numeric_prefixes`, `db::tests::quarantine_recovery_claim_renewal_extends_only_the_exact_claim` |  |
| 7 | `record_quarantine_recovery_no_progress` `db.rs:5566` | 36/37 | 2/2 | 36 | `db::tests::quarantine_recovery_stale_claim_cannot_mutate_replacement_episode`, `db::tests::sustained_cleanup_contention_slows_down` | yes (94) | yes (36) | – | `db::tests::quarantine_recovery_stale_claim_cannot_mutate_replacement_episode`, `db::tests::cleanup_contention_releases_the_claim_and_backs_off` |  |
| 8 | `publish_orphan_quarantine_recovered_artifact` `db.rs:5671` | 67/70 | 5/6 | 7 | `db::tests::publishing_a_recovered_orphan_leaves_no_quarantine_row`, `db::tests::a_publishing_orphan_restore_never_becomes_an_adoption_candidate` | yes (14) | yes (10) | 0/1 | `db::tests::every_claim_fenced_statement_pins_every_conjunct`, `db::tests::quarantine_adoption_completes_with_the_authority_receipt_id` | Secondary: `retired`≠1 refusal **never**. |
| 9 | `adopt_quarantine_cleanup_episode` `db.rs:5835` | 86/89 | 3/4 | 6 | `db::tests::every_claim_fenced_statement_pins_every_conjunct`, `db::tests::quarantine_adoption_completes_with_the_authority_receipt_id` | yes (30) | yes (45) | – | `db::tests::every_claim_fenced_statement_pins_every_conjunct`, `service::recovery::tests::two_actors_in_two_windows_still_leave_every_fenced_write_missing` | Secondary: `released`≠1 refusal **never**. |
| 10 | `mark_quarantined_inner` `db.rs:5989` | 165/172 | 20/26 | 107 | `db::tests::mark_quarantined_reports_unknown_current_state`, `db::tests::pause_failed_recovery_requires_exact_quarantine_fence` | yes (597) | **no** | 1/1 | – |  |
| 11 | `retire_quarantine_recovery` `db.rs:6897` | 34/39 | 2/4 | 1 | `db::tests::quarantine_adoption_completes_with_the_authority_receipt_id` | yes (1) | **no** | – | – | Secondary: `removed`≠1 refusal **never**. |

Read-only functions (lines covered/total): `current_quarantine_reason_wire_value` 15/23, `quarantine_recovery_observations` 52/59, `sandbox_state_publication_metadata` 38/42, `match_quarantine_recovery` 45/49, `is_orphan_quarantine_recovery_state` 24/26, `quarantine_recovery_claim_is_current` 17/18, `read_batch` 65/68, `mark_cross_node_restore_staging_failed_under_recovery_claim` 22/23, `prune_batch` 68/71, `check_state_metadata` 23/24, `retire_orphan_quarantine_restore_transfer_scope` 24/25, `set_orphan_quarantine_recovery_generation` 24/25, `restore_adopt_orphan_or_insert_sandbox_in_transaction` 146/148, `new` 32/32.


#### `local_artifacts` — 15 writers, 12 read-only functions

| # | Function (file:line) | Lines cov | Branch outcomes | # tests | Example tests | Success path | Refusal (CAS 0 rows) | Pre-check guards hit | Tests hitting refusal | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `upsert_suspend_local_artifact` `db.rs:3778` | 39/43 | 2/2 | 42 | `db::tests::restaging_the_same_teardown_artifact_preserves_its_durability_marker`, `db::tests::publish_restored_suspended_artifact_dedupes_prior_suspend_row` | yes | n/a | – | – |  |
| 2 | `delete_sandbox_child_rows` `db.rs:3901` | 23/29 | 0/0 | 36 | `db::tests::delete_sandbox_removes_local_artifact_orphans`, `db::tests::delete_sandbox_removes_network_policy_reconcile_rows` | yes | n/a | – | – |  |
| 3 | `publish_restored_suspended_artifact` `db.rs:4007` | 45/48 | 1/2 | 2 | `db::tests::publish_restored_suspended_artifact_dedupes_prior_suspend_row`, `service::recovery::tests::cross_node_restore_row_lifecycle_stays_restoring_until_rootfs_stage_succeeds` | yes (2) | **no** | – | – |  |
| 4 | `insert_local_artifact` `db.rs:4122` | 23/23 | 0/0 | 99 | `db::tests::delete_sandbox_removes_local_artifact_orphans`, `db::tests::deleting_named_snapshot_identity_does_not_delete_current_instance_artifact` | yes | n/a | – | – |  |
| 5 | `mark_local_artifact_backup_completed` `db.rs:4156` | 10/10 | 0/0 | 14 | `db::tests::restaging_the_same_teardown_artifact_preserves_its_durability_marker`, `db::tests::teardown_suspend_keeps_the_runtime_until_the_staged_artifact_is_durable` | yes | n/a | – | – |  |
| 6 | `delete_named_local_artifact` `db.rs:4367` | 11/12 | 0/0 | 1 | `db::tests::deleting_named_snapshot_identity_does_not_delete_current_instance_artifact` | yes | n/a | – | – |  |
| 7 | `check_out_for_restore` `local_artifacts.rs:537` | 39/47 | 6/8 | 12 | `local_artifacts::tests::check_out_returns_not_found_for_missing_row`, `local_artifacts::tests::restore_lease_drop_without_release_rolls_back_to_ready` | yes (15) | **no** | 2/3 | – |  |
| 8 | `checkout_delete_in_transaction` `local_artifacts.rs:681` | 45/56 | 9/12 | 11 | `local_artifacts::tests::check_out_for_delete_refuses_shared_local_path`, `local_artifacts::tests::delete_lease_commit_removes_row` | yes (9) | **no** | 3/5 | – |  |
| 9 | `recover_stuck_states` `local_artifacts.rs:804` | 60/61 | 6/6 | 5 | `local_artifacts::tests::recover_finishes_delete_when_staging_dir_present`, `local_artifacts::tests::recover_finishes_delete_when_neither_path_present` | yes | n/a | – | – |  |
| 10 | `prune_missing_orphan_rows` `local_artifacts.rs:892` | 68/72 | 11/12 | 4 | `local_artifacts::tests::prune_missing_orphan_rows_obeys_batch_limit`, `local_artifacts::tests::prune_missing_orphan_rows_preserves_unreadable_paths` | yes | n/a | – | – |  |
| 11 | `release_with_touch` `local_artifacts.rs:1047` | 10/10 | 0/0 | 5 | `local_artifacts::tests::check_out_for_restore_flips_state_and_releases_back`, `local_artifacts::tests::restore_lease_release_bumps_last_touched_at_but_drop_does_not` | yes | n/a | – | – |  |
| 12 | `release_without_touch` `local_artifacts.rs:1063` | 9/9 | 0/0 | 7 | `local_artifacts::tests::restore_lease_drop_without_release_rolls_back_to_ready`, `local_artifacts::tests::check_out_for_restore_flips_state_and_releases_back` | yes | n/a | – | – |  |
| 13 | `commit` `local_artifacts.rs:1113` | 13/14 | 1/2 | 5 | `local_artifacts::tests::delete_lease_commit_removes_row`, `local_artifacts::tests::named_publication_inventory_reports_state_without_taking_a_lease` | yes (5) | **no** | – | – |  |
| 14 | `drop` `local_artifacts.rs:1133` | 13/17 | 3/4 | 9 | `local_artifacts::tests::delete_lease_commit_removes_row`, `local_artifacts::tests::check_out_for_delete_blocks_concurrent_restore_and_vice_versa` | yes | n/a | – | – |  |
| 15 | `backfill_backup_completed_markers` `snapshot_upload_jobs.rs:1117` | 67/73 | 17/20 | 4 | `snapshot_upload_jobs::tests::backup_marker_repair_bounds_discovery_and_advances_past_retained_evidence`, `snapshot_upload_jobs::tests::backfill_stamps_unmarked_artifacts_and_buckets_the_rest` | yes (4) | yes (8) | – | `snapshot_upload_jobs::tests::backup_marker_repair_bounds_discovery_and_advances_past_retained_evidence`, `snapshot_upload_jobs::tests::backfill_stamps_unmarked_artifacts_and_buckets_the_rest` |  |

Read-only functions (lines covered/total): `list_named_publications` 21/39, `teardown_suspend_status` 67/102, `list_pending_backups` 25/31, `publish_snapshot` 80/99, `bind_suspended_teardown_operation` 30/32, `next_page` 55/58, `check_out_for_gc` 53/54, `gather_local_evidence` 57/58, `get_local_artifact` 11/11, `get_named_local_artifact` 13/13, `teardown_suspend_artifact_is_current_and_backed` 22/22, `gc_staging_is_unowned` 6/6.


#### `sandbox_restore_transfer_scopes` — 9 writers, 5 read-only functions

| # | Function (file:line) | Lines cov | Branch outcomes | # tests | Example tests | Success path | Refusal (CAS 0 rows) | Pre-check guards hit | Tests hitting refusal | Notes |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | `retire_rowless_restore_transfer_scope` `db.rs:650` | 15/16 | 0/0 | 4 | `db::cleanup_discovery_tests::recovery_scope_discovery_retains_bad_evidence_and_retires_valid_siblings`, `db::tests::orphan_quarantine_recovery_clears_staging_failure` | yes (2) | yes (2) | – | `db::tests::orphan_quarantine_recovery_clears_staging_failure`, `service::recovery::tests::cross_node_restore_row_lifecycle_stays_restoring_until_rootfs_stage_succeeds` |  |
| 2 | `retire_orphan_quarantine_restore_transfer_scope` `db.rs:667` | 24/25 | 0/0 | 1 | `db::tests::orphan_quarantine_recovery_clears_staging_failure` | yes (1) | **no** | – | – |  |
| 3 | `claim_retryable_restore_transfer_scope_if_current_or_newer` `db.rs:701` | 56/57 | 0/0 | 2 | `db::tests::authoritative_restore_attempt_claims_legacy_unowned_retryable_scope`, `db::tests::retryable_restore_scope_handoff_rejects_a_after_b_and_admits_c` | yes (4) | yes (2) | – | `db::tests::authoritative_restore_attempt_claims_legacy_unowned_retryable_scope`, `db::tests::retryable_restore_scope_handoff_rejects_a_after_b_and_admits_c` |  |
| 4 | `advance_exchanged_restore_cleanup_owner_if_newer` `db.rs:768` | 48/49 | 0/0 | 2 | `db::tests::exchanged_restore_cleanup_owner_advances_without_moving_the_scope`, `service::recovery_cross_node::tests::exchanged_restore_cleanup_failure_retains_scope_until_retry` | yes (2) | yes (2) | – | `db::tests::exchanged_restore_cleanup_owner_advances_without_moving_the_scope`, `service::recovery_cross_node::tests::exchanged_restore_cleanup_failure_retains_scope_until_retry` |  |
| 5 | `insert_restore_transfer_scope` `db.rs:852` | 21/22 | 0/0 | 20 | `db::cleanup_discovery_tests::recovery_scope_discovery_retains_bad_evidence_and_retires_valid_siblings`, `db::tests::restored_disk_as_new_becomes_a_stopped_sandbox_without_donor_runtime_identity` | yes (161) | **no** | – | – |  |
| 6 | `retire_restore_transfer_scope` `db.rs:878` | 6/7 | 0/0 | 13 | `db::tests::restored_disk_as_new_becomes_a_stopped_sandbox_without_donor_runtime_identity`, `db::tests::retryable_restore_scope_handoff_rejects_a_after_b_and_admits_c` | yes | n/a | – | – |  |
| 7 | `retire_restore_transfer_scope_if_matches` `db.rs:886` | 0/12 | 0/0 | 0 | – | **no** | **no** | – | – |  |
| 8 | `delete_sandbox_child_rows` `db.rs:3901` | 23/29 | 0/0 | 36 | `db::tests::delete_sandbox_removes_local_artifact_orphans`, `db::tests::delete_sandbox_removes_network_policy_reconcile_rows` | yes | n/a | – | – |  |
| 9 | `normalize_batch` `db/timestamps.rs:114` | 37/38 | 12/14 | 3 | `db::timestamps::tests::failed_normalization_rolls_back_and_retries_the_whole_page`, `db::timestamps::tests::scope_normalization_is_bounded_progressing_and_preserves_raw_invalid_evidence` | yes | n/a | – | – |  |

Read-only functions (lines covered/total): `rowless_restore_transfer_scopes` 20/23, `startup_resident_witness_current` 18/20, `next_batch` 45/49, `prune_batch` 68/71, `restore_transfer_scope` 12/12.

### Q2 findings

- **`sandbox_restore_transfer_scopes`:** the CAS writers' success paths are all
  covered except `retire_restore_transfer_scope_if_matches` (`db.rs:886`, 0/12).
  Its only production caller is the `retire_scope` closure in
  `service/start.rs::prepare_suspend_restore_dir`, which is also at 0 %.
  `insert_restore_transfer_scope` never refuses.
  `retire_orphan_quarantine_restore_transfer_scope` is reached by one test, and
  the `rowless_restore_transfer_scopes` guard (`db.rs:644`) is uncovered.
- **`sandbox_cleanup_episodes`:** `retire_stale_armed_cleanup_episode` is at
  13/42 lines, and the branch on its delete's result is never reached.
  `cleanup_episode` (the decoder every writer uses) is at 78/134: all of its
  invalid-row error arms are uncovered. These are unknown entered-from or
  activation state, a malformed generation or attempt, and an incomplete
  quarantine-recovery fence (`db.rs:6252`–`6314`). `bind_prepared_clone_root` and `complete_create` never refuse.
- **`sandbox_quarantine_state`:** `retire_quarantine_recovery` is single-test,
  and its `inserted`/`removed` invariants never fail.
  `current_quarantine_reason_wire_value` is at 15/23.
  `match_quarantine_recovery` has 2 uncovered refusal returns
  (`db.rs:6635`, `db.rs:6638`).
- **`local_artifacts`:** the guard-heavy checkout functions are partly covered.
  `check_out_for_restore` misses guard L556; `checkout_delete_in_transaction`
  misses guards L707 and L710; `check_out_for_gc` misses L652. Their CAS
  refusals (L575, L739) are never hit. `publish_snapshot` has 5 of 8 guard
  returns uncovered (L158, L164, L171, L183, L214).
  `list_named_publications` is at 21/39.

## Q3 — sbx-mgr service lifecycle files

| File | Functions | 0%-covered functions | Lines covered | Branch outcomes covered |
|---|---|---|---|---|
| `service/grpc_lifecycle.rs` | 90 | 14 | 2046/3830 (53.4%) | 281/542 (52%) |
| `service/cleanup.rs` | 79 | 17 | 1458/2164 (67.4%) | 141/232 (61%) |
| `service/recovery.rs` | 30 | 3 | 895/1199 (74.6%) | 99/144 (69%) |
| `service/recovery_cross_node.rs` | 40 | 7 | 774/1592 (48.6%) | 88/210 (42%) |
| `service/start.rs` | 26 | 9 | 523/1704 (30.7%) | 66/182 (36%) |
| `service/create.rs` | 16 | 8 | 76/970 (7.8%) | 4/88 (5%) |
| `snapshot_restore.rs` | 71 | 23 | 715/1579 (45.3%) | 104/168 (62%) |
| `service/activation_support.rs` | 61 | 8 | 994/1219 (81.5%) | 130/168 (77%) |

Flagged (> 40 source lines and < 50 % line coverage): 39

| file:line | Function | Source lines | Lines covered | Branch outcomes |
|---|---|---|---|---|
| `service/grpc_lifecycle.rs:250` | `pause_with_reconciliation` | 71 | 0/55 (0%) | 0/10 |
| `service/grpc_lifecycle.rs:748` | `suspend_snapshot_metadata` | 51 | 0/48 (0%) | – |
| `service/grpc_lifecycle.rs:1587` | `pause` | 109 | 0/88 (0%) | 0/10 |
| `service/grpc_lifecycle.rs:2985` | `drive_teardown_suspend_inner` | 103 | 42/91 (46%) | 3/8 |
| `service/grpc_lifecycle.rs:3571` | `complete_suspend_snapshot_publication` | 609 | 0/411 (0%) | 0/48 |
| `service/grpc_lifecycle.rs:4181` | `finalize_suspended_runtime` | 66 | 0/59 (0%) | 0/8 |
| `service/grpc_lifecycle.rs:4396` | `existing_ready_artifact_backup_response` | 63 | 0/35 (0%) | 0/4 |
| `service/grpc_lifecycle.rs:4488` | `enqueue_artifact_backup` | 365 | 42/275 (15%) | 4/34 |
| `service/grpc_lifecycle.rs:4893` | `enqueue_suspend_snapshot_reupload` | 197 | 0/99 (0%) | 0/8 |
| `service/grpc_lifecycle.rs:5107` | `resolve_org_snapshot_upload_config` | 68 | 7/34 (21%) | 0/4 |
| `service/grpc_lifecycle.rs:5425` | `establish_restore_visible_artifact_backup` | 61 | 19/46 (41%) | 2/8 |
| `service/grpc_lifecycle.rs:5546` | `kill` | 180 | 70/150 (47%) | 8/20 |
| `service/cleanup.rs:115` | `into_status` | 48 | 8/24 (33%) | – |
| `service/cleanup.rs:650` | `cancel_restore_transfer_scope_and_remove_staging` | 56 | 15/45 (33%) | 1/4 |
| `service/cleanup.rs:726` | `retire_rowless_restore_transfer_scope` | 43 | 0/34 (0%) | 0/6 |
| `service/cleanup.rs:1870` | `collect_net_gateway_stats` | 82 | 0/57 (0%) | 0/6 |
| `service/cleanup.rs:2764` | `run_cleanup_after_persistence_failure` | 45 | 0/42 (0%) | – |
| `service/recovery.rs:295` | `recover_runtime_registrations` | 60 | 0/12 (0%) | – |
| `service/recovery.rs:2048` | `start_inventory_reconciler` | 103 | 0/89 (0%) | 0/16 |
| `service/recovery_cross_node.rs:506` | `snapshot_restore_event_details` | 47 | 0/41 (0%) | 0/14 |
| `service/recovery_cross_node.rs:1124` | `claim_and_clean_retryable_restore_transfer_scope` | 77 | 0/62 (0%) | 0/8 |
| `service/recovery_cross_node.rs:1202` | `run_cross_node_restore_inner` | 913 | 133/687 (19%) | 20/96 |
| `service/recovery_cross_node.rs:2227` | `publish_rebound_sandbox_artifact_metadata` | 55 | 26/54 (48%) | – |
| `service/start.rs:365` | `settle_net_gateway_launch_task` | 46 | 0/21 (0%) | – |
| `service/start.rs:432` | `prepare_suspend_restore_dir` | 117 | 0/103 (0%) | 0/4 |
| `service/start.rs:675` | `node_sandbox_memory_policy` | 54 | 0/44 (0%) | 0/2 |
| `service/start.rs:825` | `materialize_missing_checkpoint_restore` | 197 | 43/140 (31%) | – |
| `service/start.rs:1023` | `restore_suspended_checkpoint_start` | 320 | 114/231 (49%) | 14/28 |
| `service/start.rs:1344` | `try_start_from_warm_snapshot` | 111 | 0/92 (0%) | 0/10 |
| `service/start.rs:1476` | `start_sandbox_with_authority` | 1022 | 92/685 (13%) | 16/74 |
| `service/create.rs:265` | `create_with_id` | 322 | 0/262 (0%) | 0/36 |
| `service/create.rs:601` | `start_from_checkpoint` | 747 | 0/539 (0%) | 0/36 |
| `snapshot_restore.rs:1395` | `quarantine_recovery_plan_inner` | 52 | 0/43 (0%) | 0/6 |
| `snapshot_restore.rs:1673` | `restore_pull_metadata_from_source_for_identity` | 117 | 24/79 (30%) | 0/2 |
| `snapshot_restore.rs:1810` | `restore_pull_rootfs` | 77 | 0/57 (0%) | 0/8 |
| `snapshot_restore.rs:2071` | `pull_artifact_inner` | 146 | 0/105 (0%) | 0/2 |
| `snapshot_restore.rs:2333` | `stage_full_codec_artifacts` | 60 | 0/48 (0%) | 0/2 |
| `snapshot_restore.rs:2456` | `stage_btrfs_send_restore` | 94 | 0/62 (0%) | 0/2 |
| `snapshot_restore.rs:2599` | `ensure_local_template` | 44 | 0/38 (0%) | 0/2 |

Q3 notes:
- Every `update_state` call site in `grpc_lifecycle.rs` (L1620, L1634, L1677,
  L3414, L3426, L3486), `create.rs` (L416, L528), `start.rs:1426`,
  `cleanup.rs:1538`, `auth_lookup.rs:812`, `activation_support.rs:1811` and the
  snapshot files is unreached (see the Q1 call-site table).
- `create.rs::start_from_checkpoint` (747 lines) and
  `start.rs::start_sandbox_with_authority` (1,022 lines, 13 %) are the largest
  single blocks of unexecuted lifecycle code. Many of these paths reach
  node-mgr through `connect_node()`, a fixed socket path with no injection seam.

## Q4 — node-mgr slot pool, service, snapshots, launch, runtime, limiter

| File | Functions | 0%-covered functions | Lines covered | Branch outcomes covered |
|---|---|---|---|---|
| `slot_pool.rs` | 75 | 11 | 973/1264 (77.0%) | 85/128 (66%) |
| `slot_pool/db.rs` | 31 | 1 | 533/629 (84.7%) | 43/60 (72%) |
| `slot_pool/execution.rs` | 3 | 0 | 46/53 (86.8%) | – |
| `slot_pool/liveness.rs` | 7 | 0 | 96/105 (91.4%) | 31/34 (91%) |
| `slot_pool/reset.rs` | 23 | 6 | 240/617 (38.9%) | 36/128 (28%) |
| `slot_pool/scheduler.rs` | 5 | 1 | 94/138 (68.1%) | 10/40 (25%) |
| `slot_pool/startup_containment.rs` | 2 | 1 | 44/79 (55.7%) | 10/18 (56%) |
| `slot_pool/time.rs` | 1 | 0 | 6/6 (100.0%) | – |
| `slot_pool/worker.rs` | 34 | 3 | 724/1034 (70.0%) | 91/152 (60%) |
| `service.rs` | 38 | 15 | 230/380 (60.5%) | 11/20 (55%) |
| `service/cleanup_inspect.rs` | 17 | 3 | 412/674 (61.1%) | 35/98 (36%) |
| `service/snapshots.rs` | 38 | 18 | 347/895 (38.8%) | 35/90 (39%) |
| `service/launch_prepare.rs` | 17 | 8 | 126/733 (17.2%) | 6/96 (6%) |
| `service/runtime.rs` | 20 | 13 | 130/915 (14.2%) | 8/58 (14%) |
| `lifecycle_limiter.rs` | 17 | 1 | 173/188 (92.0%) | 9/12 (75%) |

Flagged (> 40 source lines and < 50 % line coverage): 25

| file:line | Function | Source lines | Lines covered | Branch outcomes |
|---|---|---|---|---|
| `slot_pool.rs:685` | `open` | 69 | 0/63 (0%) | 0/2 |
| `slot_pool/db.rs:390` | `apply_runtime_reconcile_updates` | 89 | 30/79 (38%) | 1/2 |
| `slot_pool/reset.rs:177` | `reset_slot` | 548 | 107/326 (33%) | 19/64 |
| `slot_pool/reset.rs:834` | `validate_slot_ready` | 103 | 0/99 (0%) | 0/30 |
| `slot_pool/scheduler.rs:7` | `spawn_worker` | 72 | 23/56 (41%) | 3/30 |
| `slot_pool/worker.rs:715` | `create_slot` | 135 | 11/97 (11%) | 2/18 |
| `slot_pool/worker.rs:1178` | `validate_and_apply_recoverable_idle_records` | 61 | 18/53 (34%) | 0/2 |
| `slot_pool/worker.rs:1247` | `validate_recoverable_idle_record_with` | 70 | 28/57 (49%) | 3/6 |
| `service.rs:288` | `with_memory_admission_config_and_task_tracker` | 74 | 0/71 (0%) | – |
| `service/cleanup_inspect.rs:320` | `cleanup_sandbox_phases` | 360 | 87/266 (33%) | 4/42 |
| `service/snapshots.rs:288` | `create_snapshot_inner` | 108 | 0/75 (0%) | 0/4 |
| `service/snapshots.rs:404` | `create_encrypted_snapshot` | 142 | 0/83 (0%) | 0/2 |
| `service/snapshots.rs:569` | `load_snapshot_inner` | 106 | 29/86 (34%) | 1/4 |
| `service/snapshots.rs:680` | `finish_load_snapshot` | 75 | 0/56 (0%) | 0/14 |
| `service/snapshots.rs:995` | `rollback_btrfs_snapshot_inner` | 59 | 13/44 (30%) | 0/6 |
| `service/launch_prepare.rs:122` | `sandbox_metadata_json` | 52 | 0/45 (0%) | 0/4 |
| `service/launch_prepare.rs:252` | `launch_sandbox_inner` | 389 | 0/280 (0%) | 0/42 |
| `service/launch_prepare.rs:656` | `prepare_sandbox_inner` | 219 | 12/164 (7%) | 1/32 |
| `service/runtime.rs:23` | `firecracker_before_jail_rebuild` | 58 | 11/52 (21%) | 3/10 |
| `service/runtime.rs:178` | `allocate_inner` | 266 | 24/185 (13%) | 1/8 |
| `service/runtime.rs:460` | `launch_inner` | 315 | 0/264 (0%) | 0/18 |
| `service/runtime.rs:791` | `release_inner` | 114 | 21/93 (23%) | 2/8 |
| `service/runtime.rs:996` | `ensure_loop_capacity_for_slot_increase` | 47 | 0/28 (0%) | 0/2 |
| `service/runtime.rs:1044` | `set_network_pool_config` | 49 | 0/27 (0%) | – |
| `service/runtime.rs:1108` | `stop_process_inner` | 105 | 0/73 (0%) | 0/6 |

### Per-sandbox mutation claim and "retry" contention paths

The core claim primitive is covered: `slot_pool.rs::try_claim_sandbox_mutation`
ran 552 times and was busy 25 times. The async waiter `claim_sandbox_mutation`
waited and retried 4 times, in the 4 `execution_preflight_waits_for_cleanup_and_revalidates`
cases. The `Status::unavailable("sandbox host mutation or reset is in progress; retry")`
mapping in `service.rs:524` ran 21 times across 5 tests, for example
`service::filesystem_work::tests::artifact_mutation_cancellation_retains_target_until_blocking_work_exits`
and `service::tests::service_tests::cancelled_clone_retains_both_cleanup_gates_until_blocking_work_finishes`.
The `wait_for_cleanup_phase` timeout ("timed out waiting …; retry") ran 2 times.
Per call site:

| Call site | RPC / function | Successful claims | Contention returns |
|---|---|---|---|
| `service.rs:534` | `run_owned_sandbox_mutation` | 8 | **0** |
| `service.rs:543` | `run_owned_sandbox_work` (wait) | 6 | **0** |
| `service.rs:553` | `claim_sandbox_work` → "cleanup or reset is in progress; retry" | 11 | **0** (closure never ran) |
| `service/btrfs_delegation.rs:986` | `btrfs_delete_sandbox_instance` | 1 | 4 |
| `service/cleanup_inspect.rs:342` | `cleanup_sandbox_phases` | 17 | **0** |
| `service/control.rs:238`, `:239` | `run_owned_clone` (first/second) | 2 / 2 | **0** / **0** |
| `service/control.rs:274`, `:431`, `:501`, `:596` | `expose_port`, `unexpose_port`, `allow_port`, `deny_port` | **0** (RPCs never called) | **0** |
| `service/encrypted_volumes.rs:68` | `run_owned_device_operation` | 3 | **0** |
| `service/filesystem_work.rs:75` | `run_artifact_mutation` | 13 | 4 |
| `service/runtime.rs:171` | `allocate` (wait) | 2 | **0** |
| `service/snapshots.rs:860` | `delete_btrfs_snapshot` | **0** | 2 |
| `service/snapshots.rs:913` | `delete_cache_artifact` | **0** | 2 |
| `slot_pool/db.rs:725` | `apply_startup_lease_updates` (`let … else`) | 38 | **0** |
| `slot_pool/reset.rs:212` | `reset_slot` (`let … else`) | 7 | **0** |
| `slot_pool/worker.rs:1028` | `reprobe_startup_leasing_with` (`let … else`) | 9 | **0** |

Contention is pinned at the primitive and at 4 of 15 RPC call sites.
`delete_btrfs_snapshot` and `delete_cache_artifact` are tested only for
contention: their success paths never ran. The startup and reset `let … else`
contention exits in `slot_pool` are never taken.

## Q5 — api Kubernetes substrate and sandbox lifecycle

| File | Functions | 0%-covered functions | Lines covered | Branch outcomes covered |
|---|---|---|---|---|
| `controllers/substrate/kubernetes/store.rs` | 127 | 7 | 3500/3772 (92.8%) | 240/296 (81%) |
| `controllers/substrate/kubernetes/adapter.rs` | 74 | 25 | 1438/2984 (48.2%) | 129/324 (40%) |
| `controllers/substrate/kubernetes/pod_ops.rs` | 81 | 9 | 2106/2906 (72.5%) | 317/502 (63%) |
| `controllers/substrate/kubernetes/execution.rs` | 35 | 0 | 1095/1261 (86.8%) | 116/162 (72%) |
| `controllers/sandbox/lifecycle.rs` | 52 | 15 | 790/1453 (54.4%) | 55/108 (51%) |
| `controllers/sandbox/wake_completion.rs` | 13 | 0 | 173/216 (80.1%) | 17/18 (94%) |

Flagged (> 40 source lines and < 50 % line coverage): 17

| file:line | Function | Source lines | Lines covered | Branch outcomes |
|---|---|---|---|---|
| `controllers/substrate/kubernetes/adapter.rs:773` | `apply_adopted_per_sandbox_setup` | 251 | 0/123 (0%) | 0/18 |
| `controllers/substrate/kubernetes/adapter.rs:1042` | `preservable_create_node` | 58 | 0/45 (0%) | 0/14 |
| `controllers/substrate/kubernetes/adapter.rs:1248` | `teardown_execution` | 280 | 83/175 (47%) | 13/34 |
| `controllers/substrate/kubernetes/adapter.rs:1857` | `create_execution` | 646 | 170/393 (43%) | 6/32 |
| `controllers/substrate/kubernetes/adapter.rs:2605` | `delete_observed_execution` | 78 | 13/53 (25%) | 0/6 |
| `controllers/substrate/kubernetes/adapter.rs:3027` | `suspend_execution` | 197 | 0/130 (0%) | 0/2 |
| `controllers/substrate/kubernetes/adapter.rs:3341` | `access_runtime_client` | 91 | 26/73 (36%) | 2/12 |
| `controllers/substrate/kubernetes/adapter.rs:4399` | `prepare_pooled_execution` | 305 | 0/220 (0%) | 0/22 |
| `controllers/substrate/kubernetes/pod_ops.rs:516` | `drop` | 64 | 4/36 (11%) | 1/8 |
| `controllers/substrate/kubernetes/pod_ops.rs:1917` | `hydrate_restored_execution` | 66 | 0/44 (0%) | 0/8 |
| `controllers/substrate/kubernetes/pod_ops.rs:2840` | `remember_pod_node_placement` | 60 | 9/49 (18%) | 1/4 |
| `controllers/substrate/kubernetes/pod_ops.rs:2901` | `delete_pod_and_forget_tracking` | 43 | 16/33 (48%) | – |
| `controllers/substrate/kubernetes/pod_ops.rs:2999` | `resolve_node_group_placement_for_claim` | 42 | 9/20 (45%) | 0/2 |
| `controllers/sandbox/lifecycle.rs:1205` | `delete_observed_sandbox` | 41 | 0/33 (0%) | – |
| `controllers/sandbox/lifecycle.rs:1476` | `action` | 671 | 213/457 (47%) | 19/40 |
| `controllers/sandbox/lifecycle.rs:2148` | `untracked_action` | 177 | 0/147 (0%) | – |
| `controllers/sandbox/wake_completion.rs:246` | `wake_completion_redrive` | 63 | 9/39 (23%) | – |

## Ranked gaps: top 40 uncovered high-churn functions

Churn weight: the task supplied the ranked list of files with the most bug-fix
commits last month, and the public history of this repository starts at the
2026-10-05 import. So the weight is `19 − rank` (db.rs = 18 … slot_pool.rs = 1).
spaced-guest `process.rs` (rank 6) is outside the four measured packages and is
excluded. Public post-import commit counts agree with the ranking's order:
grpc_lifecycle.rs 6, db.rs 5, node-mgr snapshots.rs 5, the rest 0–3.
Score = weight × adjusted uncovered lines. The last two columns come from
reading each function and its sibling test modules.

| Rank | Function | file:line | Churn rank (weight) | Lines covered | Uncovered (adj.) | Branch outcomes | Score | Blocking dependency | Lowest feasible test layer |
|---|---|---|---|---|---|---|---|---|---|
| 1 | `complete_suspend_snapshot_publication` | `sbx-mgr src/service/grpc_lifecycle.rs:3571` | 2 (17) | 0/411 (0%) | 354 | 0/48 | 6018 | node-mgr RPC via fixed-socket `connect_node` + spaced-host | needs fake + client-injection seam |
| 2 | `enqueue_artifact_backup` | `sbx-mgr src/service/grpc_lifecycle.rs:4488` | 2 (17) | 42/275 (15%) | 186 | 4/34 | 3162 | cluster-api env, node-mgr, DB jobs | unit (existing `grpc_lifecycle/tests.rs` env pattern) |
| 3 | `launch_sandbox_inner` | `node-mgr src/service/launch_prepare.rs:252` | 8 (11) | 0/280 (0%) | 238 | 0/42 | 2618 | Firecracker/jailer via runtime-supervisor | unit for request validation (`StubRuntimeSupervisor`); launch itself Kind |
| 4 | `create` | `sbx-mgr src/service/grpc_core.rs:386` | 9 (10) | 32/267 (12%) | 190 | 5/38 | 1900 | template-mgr gRPC (fixed socket), then node-mgr | unit up to template lookup; template-mgr fake after |
| 5 | `launch_inner` | `node-mgr src/service/runtime.rs:460` | 11 (8) | 0/264 (0%) | 229 | 0/18 | 1832 | runtime-supervisor `launch_sidecar`, netns | unit for allowlist/lease/uid checks |
| 6 | `prepare_sandbox_inner` | `node-mgr src/service/launch_prepare.rs:656` | 8 (11) | 12/164 (7%) | 134 | 1/32 | 1474 | btrfs/loop/devices | unit for validation; Kind for filesystem work |
| 7 | `enqueue_suspend_snapshot_reupload` | `sbx-mgr src/service/grpc_lifecycle.rs:4893` | 2 (17) | 0/99 (0%) | 81 | 0/8 | 1377 | cluster-api env + DB job submit | unit (existing harness) |
| 8 | `cleanup_sandbox_phases` | `node-mgr src/service/cleanup_inspect.rs:320` | 10 (9) | 87/266 (33%) | 150 | 4/42 | 1350 | host processes, supervisor stop, device teardown | unit (`cleanup_inspect/tests.rs` `stop_failure_client`) |
| 9 | `prepare_suspend_snapshot_publication_after_locks` | `sbx-mgr src/service/grpc_lifecycle.rs:3307` | 2 (17) | 96/187 (51%) | 73 | 15/26 | 1241 | node-mgr pause + spaced-host (Paused rows DB-only) | unit (existing `service/tests.rs` suspend tests) |
| 10 | `allocate_inner` | `node-mgr src/service/runtime.rs:178` | 11 (8) | 24/185 (13%) | 152 | 1/8 | 1216 | in-memory NetworkPool + memory admission | unit (existing; fake meminfo pattern) |
| 11 | `pause` | `sbx-mgr src/service/grpc_lifecycle.rs:1587` | 2 (17) | 0/88 (0%) | 71 | 0/10 | 1207 | spaced-host + node-mgr via `connect_node` | unit for spaced-host-failure rollback; node path needs seam |
| 12 | `create_encrypted_snapshot` | `node-mgr src/service/snapshots.rs:404` | 3 (16) | 0/83 (0%) | 72 | 0/2 | 1152 | Firecracker API + jail bind mounts | unit for validation only; Kind |
| 13 | `start_inventory_reconciler` | `sbx-mgr src/service/recovery.rs:2048` | 5 (14) | 0/89 (0%) | 82 | 0/16 | 1148 | fixed CACHE_DIR, lazy node-mgr client, 60 s loop | unit with paused Tokio time |
| 14 | `kill` | `sbx-mgr src/service/grpc_lifecycle.rs:5546` | 2 (17) | 70/150 (47%) | 65 | 8/20 | 1105 | node-mgr `stop_process` | unit (fatal path via cleanup-dispatch seam); non-fatal needs fake |
| 15 | `create_snapshot_inner` | `node-mgr src/service/snapshots.rs:288` | 3 (16) | 0/75 (0%) | 65 | 0/4 | 1040 | Firecracker API socket under JAIL_DIR | needs FC-socket path seam |
| 16 | `finalize_suspended_runtime` | `sbx-mgr src/service/grpc_lifecycle.rs:4181` | 2 (17) | 0/59 (0%) | 52 | 0/8 | 884 | cleanup-dispatch seam + watchdog | unit (existing harness) |
| 17 | `pause_with_reconciliation` | `sbx-mgr src/service/grpc_lifecycle.rs:250` | 2 (17) | 0/55 (0%) | 50 | 0/10 | 850 | node-mgr client passed as parameter | unit (`grpc_pool_wait_clone/tests.rs` `CleanupPeer` fake) |
| 18 | `load_snapshot_inner` | `node-mgr src/service/snapshots.rs:569` | 3 (16) | 29/86 (34%) | 51 | 1/4 | 816 | Firecracker + jail mounts | unit for validation; Kind |
| 19 | `set_network_policy` | `sbx-mgr src/service/grpc_lifecycle.rs:2236` | 2 (17) | 97/161 (60%) | 47 | 17/32 | 799 | net-gateway liveness + DB | unit (existing harness) |
| 20 | `finish_load_snapshot` | `node-mgr src/service/snapshots.rs:680` | 3 (16) | 0/56 (0%) | 47 | 0/14 | 752 | Firecracker API | Kind (or FC-socket seam) |
| 21 | `with_memory_admission_config_and_task_tracker` | `node-mgr src/service.rs:288` | 7 (12) | 0/71 (0%) | 61 | – | 732 | `NetworkPool::open` fixed DB path, netns containment | Kind only |
| 22 | `drive_teardown_suspend_inner` | `sbx-mgr src/service/grpc_lifecycle.rs:2985` | 2 (17) | 42/91 (46%) | 43 | 3/8 | 731 | node-mgr, upload worker, cleanup dispatch | unit (existing `grpc_lifecycle/tests.rs`) |
| 23 | `suspend_snapshot_metadata` | `sbx-mgr src/service/grpc_lifecycle.rs:748` | 2 (17) | 0/48 (0%) | 41 | – | 697 | none (fixed host metadata path, default fallback) | unit |
| 24 | `record_suspend_snapshot_publication_failure_after_staging` | `sbx-mgr src/service/grpc_lifecycle.rs:1074` | 2 (17) | 49/92 (53%) | 41 | 8/22 | 697 | none (DB + tempdir, injectable liveness) | unit (existing harness) |
| 25 | `delete_with_auto_lifecycle_expectation` | `sbx-mgr src/service/grpc_core.rs:929` | 9 (10) | 93/178 (52%) | 66 | 8/36 | 660 | DB + cleanup dispatch | unit; the `Some(expectation)` path is called only from `reconcile.rs` |
| 26 | `cleanup_episode` | `sbx-mgr src/db.rs:6230` | 1 (18) | 78/134 (58%) | 36 | – | 648 | none (DB-only) | unit |
| 27 | `resume_after_wake_lock` | `sbx-mgr src/service/auth_lookup.rs:751` | 13 (6) | 25/144 (17%) | 96 | 0/12 | 576 | net-gateway policy, node-mgr, spaced-host | unit up to the policy gate; node path needs fake |
| 28 | `adopt_quarantine_recovery_episodes_with_shutdown` | `sbx-mgr src/service/recovery.rs:1109` | 5 (14) | 206/250 (82%) | 40 | 25/28 | 560 | cluster-api plan lookup (parameter) | unit (existing `recovery/tests.rs`) |
| 29 | `clear_local_network_policy` | `sbx-mgr src/db.rs:10520` | 1 (18) | 0/44 (0%) | 30 | 0/10 | 540 | none (DB-only) | unit |
| 30 | `stop_process_inner` | `node-mgr src/service/runtime.rs:1108` | 11 (8) | 0/73 (0%) | 65 | 0/6 | 520 | runtime-supervisor stop + pidfd/cgroup checks | unit (stub returns Unimplemented; extend stub) |
| 31 | `recover_required_cleanup_targets_from_rows` | `sbx-mgr src/service/recovery.rs:1823` | 5 (14) | 107/145 (74%) | 37 | 26/30 | 518 | none (DB-only) | unit |
| 32 | `existing_ready_artifact_backup_response` | `sbx-mgr src/service/grpc_lifecycle.rs:4396` | 2 (17) | 0/35 (0%) | 30 | 0/4 | 510 | cluster-api gRPC from env | needs fake cluster-api server |
| 33 | `stop` | `sbx-mgr src/service/grpc_core.rs:796` | 9 (10) | 48/111 (43%) | 50 | 4/14 | 500 | DB + cleanup dispatch | unit (existing harness) |
| 34 | `release_inner` | `node-mgr src/service/runtime.rs:791` | 11 (8) | 21/93 (23%) | 61 | 2/8 | 488 | supervisor stop + pid liveness | unit (existing harness) |
| 35 | `rollback_activation_episode` | `sbx-mgr src/db.rs:3686` | 1 (18) | 0/55 (0%) | 27 | 0/8 | 486 | none (DB-only) | unit; no test reaches its wrappers |
| 36 | `refresh_route_ed25519_verification_bundle` | `sbx-mgr src/service/mod.rs:1013` | 4 (15) | 0/38 (0%) | 32 | – | 480 | env / file read | unit (`enter_test_process`) |
| 37 | `submit_artifact_backup` | `sbx-mgr src/service/grpc_lifecycle.rs:5251` | 2 (17) | 92/123 (75%) | 28 | 3/6 | 476 | DB jobs + fs; remote handoffs need cluster-api/volume-manager | unit for default handoff |
| 38 | `release_execution` | `sbx-mgr src/service/grpc_core.rs:182` | 9 (10) | 104/160 (65%) | 44 | 5/12 | 440 | DB + cleanup-executor seam | unit (existing harness) |
| 39 | `adopt_authoritative_route_identity` | `sbx-mgr src/service/mod.rs:1124` | 4 (15) | 118/154 (77%) | 29 | 25/28 | 435 | none (DB + in-memory epochs) | unit (existing harness) |
| 40 | `suspend` | `sbx-mgr src/service/grpc_lifecycle.rs:3180` | 2 (17) | 64/95 (67%) | 25 | 7/14 | 425 | node-mgr + spaced-host | unit before the node calls |

## Recommended characterization-test tasks (parallel, disjoint files)

Each task adds tests only. It owns the test files listed, plus the one
`#[cfg(test)] mod …;` line that wires each new test file in. That line goes in
the production file, or in the parent test file where noted. No two tasks write
the same file. T1/T2 and T4/T5 characterize disjoint line ranges of the same
production file. Tasks marked *seam* need a small production change (client
injection) and should run after or alongside the matching refactor rather than
in a tests-only wave.

| # | Task | Production code characterized | Test files owned | Feasible layer | Covers ranks |
|---|---|---|---|---|---|
| T1 | Refusal paths for state writers in activation, suspend, restore, publish and eviction (`db.rs` L2153–4300, L9339–9560) | `db.rs` | new `db/state_writer_refusal_tests.rs` | unit (`db::test_conn`) | 35 + Q1 rows 6, 9, 13–21, 36 |
| T2 | Refusal paths for cleanup-episode and quarantine writers (`db.rs` L4746–7720) and the cleanup-episode decoder | `db.rs` (disjoint line range from T1) | new `db/tests/cleanup_quarantine_refusal.rs`, declared from `db/tests.rs` | unit | 26, 29 + Q1 rows 22–32 |
| T3 | `local_artifacts.rs` guards and CAS refusals (checkout, GC, publish_snapshot, drop) | `local_artifacts.rs` | new `local_artifacts/refusal_tests.rs` | unit | Q2 local_artifacts |
| T4 | sbx-mgr suspend pipeline: `pause_with_reconciliation`, `finalize_suspended_runtime`, `suspend_snapshot_metadata`, publication failure, `prepare_suspend_snapshot_publication_after_locks` (Paused rows), `suspend` up to the node call | `service/grpc_lifecycle.rs` (suspend half) | existing `service/grpc_lifecycle/tests.rs` | unit (existing fakes); *seam* for #1 and #11 | 1*, 9, 11*, 16, 17, 22, 23, 24, 40 |
| T5 | sbx-mgr artifact backup, network policy and kill: `enqueue_artifact_backup`, `enqueue_suspend_snapshot_reupload`, `submit_artifact_backup`, `set/get_network_policy`, `kill` | `service/grpc_lifecycle.rs` (backup/policy half) | new `service/grpc_lifecycle/backup_policy_tests.rs` | unit; #32 needs a fake cluster-api server | 2, 7, 14, 19, 32, 37 |
| T6 | sbx-mgr core RPCs: `grpc_core::{stop, release_execution, delete_with_auto_lifecycle_expectation}`, `create` up to template lookup | `service/grpc_core.rs` | `service/grpc_core/tests.rs` | unit | 4 (partial), 25, 33, 38 |
| T7 | sbx-mgr create/start: `rollback_starting_episode`/`rollback_restoring_episode` callers, `prepare_suspend_restore_dir` (reaches `retire_restore_transfer_scope_if_matches`), `try_start_from_warm_snapshot`, `materialize_missing_checkpoint_restore` | `service/start.rs`, `service/create.rs` | `service/start/tests.rs`; new `service/create/tests.rs` | unit; `create_with_id`/`start_from_checkpoint` need a template-mgr fake | Q3 create/start |
| T8 | sbx-mgr recovery: `start_inventory_reconciler` (paused time), `recover_runtime_registrations`, `claim_and_clean_retryable_restore_transfer_scope`, `run_cross_node_restore_inner` refusal arms | `service/recovery.rs`, `service/recovery_cross_node.rs` | `service/recovery/tests.rs`, `service/recovery_cross_node/tests.rs` | unit | 13, 28, 31 |
| T9 | sbx-mgr route identity and wake: `refresh_route_ed25519_verification_bundle`, `adopt_authoritative_route_identity`, `auth_lookup::resume_after_wake_lock` up to its policy gate | `service/mod.rs`, `service/auth_lookup.rs` | `service/tests.rs` is shared and large, so use a new `service/route_identity_tests.rs` | unit | 27, 36, 39 |
| T10 | node-mgr claim contention: one contention case per claiming RPC (table above), the `claim_sandbox_work` busy mapping, `control.rs` port RPCs, `slot_pool` startup/reset `let … else` exits | `service.rs`, `service/control.rs`, `service/encrypted_volumes.rs`, `slot_pool/db.rs` | new `service/claim_contention_tests.rs` | unit (`service_for_unit_tests`) | Q4 contention |
| T11 | node-mgr runtime and cleanup: `allocate_inner`, `release_inner`, `stop_process_inner` (extend `StubRuntimeSupervisor`), `cleanup_sandbox_phases` error arms | `service/runtime.rs`, `service/cleanup_inspect.rs` | new `service/runtime/tests.rs`; `service/cleanup_inspect/tests.rs` | unit | 8, 10, 30, 34 |
| T12 | node-mgr snapshots and launch validation: request/lease validation in `create_snapshot_inner`, `create_encrypted_snapshot`, `load_snapshot_inner`, `launch_sandbox_inner`, `prepare_sandbox_inner`, `launch_inner` | `service/snapshots.rs`, `service/launch_prepare.rs` | `service/snapshots/tests.rs`, `service/launch_prepare/tests.rs` | unit for validation; beyond that Kind or an FC-socket *seam* | 3, 5, 6, 12, 15, 18, 20 (partial) |
| T13 | node-mgr slot pool: `reset_slot` arms, `validate_slot_ready`, `create_slot`, `spawn_worker`, `apply_runtime_reconcile_updates` | `slot_pool/reset.rs`, `slot_pool/worker.rs`, `slot_pool/scheduler.rs` | new file under `slot_pool/tests/`, declared from `slot_pool/tests.rs` | unit | Q4 slot_pool |
| T14 | api Kubernetes adapter: `suspend_execution`, `prepare_pooled_execution`, `apply_adopted_per_sandbox_setup`, `create_execution`/`teardown_execution` error arms, `pod_ops::hydrate_restored_execution` | `controllers/substrate/kubernetes/{adapter,pod_ops}.rs` | new `kubernetes/adapter_characterization_tests.rs` | unit (existing fake kube store, Postgres fixture) | Q5 |
| T15 | api sandbox lifecycle: `untracked_action`, `delete_observed_sandbox`, `action` refusal arms, `wake_completion_redrive` | `controllers/sandbox/{lifecycle,wake_completion}.rs` | new `controllers/sandbox/lifecycle_characterization_tests.rs` | unit (Postgres fixture) | Q5 |

`*` = needs a seam. Rank 21 (`with_memory_admission_config_and_task_tracker`)
is Kind-only and is left to smoke coverage.

## Open questions

1. **Churn weights.** I used the supplied ranking (weight `19 − rank`) because
   the public history starts on 2026-10-05. If per-file fix counts are
   available, rerun the ranking: it is one multiplication per row of the CSV.
2. **spaced-guest `process.rs`** (churn rank 6) is outside the four packages and
   was not measured. Should the next wave include `pplx-space-spaced-guest-server`?
3. **Seams for `connect_node()`, Firecracker and template-mgr sockets.** The
   biggest gaps (ranks 1, 4, 11, 15, 20) cannot be reached by tests-only tasks.
   Should the consolidation wave add client-injection seams first, or should
   these paths stay smoke-covered?
4. **Root-only ignored tests** (3) were not run here. CI runs them as root, but
   their code is outside this report's scope.
5. **Toolchain.** Branch coverage needs `RUSTC_BOOTSTRAP=1` on the pinned
   compiler, and the pinned LLVM 22.1.8 `llvm-cov` crashes reading these
   binaries. A CI coverage job would need a newer `llvm-cov` reader, or line
   coverage only.

## Reproduction

```sh
cargo install cargo-llvm-cov --locked && rustup component add llvm-tools-preview
rustup toolchain install nightly --profile minimal --component llvm-tools-preview   # reader only
eval "$(RUSTC_BOOTSTRAP=1 cargo llvm-cov show-env --branch --sh)"; export RUSTC_BOOTSTRAP=1
export SPACE_CI_REQUIRE_TEST_DEPS=1
cargo build --locked -p pplx-space-db-test-fixtures --bin space-nextest-postgres-setup
for p in pplx-space-node-runtime-supervisor pplx-space-node-mgr pplx-space-sbx-mgr pplx-space-api; do
  cargo nextest run --locked --all-targets --all-features -p "$p" --run-ignored all --retries 1 --no-fail-fast
done
LLVM_COV="$(rustc +nightly --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-cov" \
  cargo llvm-cov report --branch --json --output-path coverage.json
```

To get per-test attribution, set a `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUNNER`
wrapper that rewrites `LLVM_PROFILE_FILE` to include a hash of
`$NEXTEST_BINARY_ID::$NEXTEST_TEST_NAME`. Then merge and export each test's
profiles separately.
