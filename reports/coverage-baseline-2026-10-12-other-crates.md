# Coverage baseline for the other sandbox-lifecycle crates (2026-10-12)

This continues the 2026-10-10 baseline (`ac/wave0-coverage-baseline`,
`reports/coverage-baseline-2026-10-10.md`), which covered sbx-mgr, node-mgr, api
and the runtime supervisor. This report measures spaced (guest and host),
net-gateway, cluster-controller, internal-api and vol-mgr, and re-measures the
runtime supervisor. It is measurement only: no production or test code
changed. All numbers come from local runs at commit
`bea0d3f2bc8d82ab11ccc1e7017b899bd3b3d1e2`, with the tests split as CI splits
them since SPACE#307: the default set unprivileged, and `group(privileged)`
privileged.

Companion data: [`coverage-baseline-2026-10-12-other-crates.csv`](coverage-baseline-2026-10-12-other-crates.csv).
Columns are `package,file,function,lines,covered_lines,adjusted_uncovered,branch_outcomes_covered,branch_outcomes,branch_pct`.
It has one `(file)` row per source file, followed by one row per source
function in that file, for all 27 packages. `function` is `name@line`, where
the line is the start of the `fn` item, including its doc comments and
attributes. `file` is relative to the package root.

Two stores share table names. The report always names which one it means:
- **Postgres** is the control-plane store: `public.sandboxes` with its
  `lifecycle` column, and the central `sandbox_quarantine_state`/`_history`.
  internal-api and cluster-controller write it.
- **sbx-mgr SQLite** is the node-local store: `sandboxes` with its `state`
  column, and the node-local quarantine tables.

None of the crates measured here writes sbx-mgr SQLite. internal-api promotes
Postgres `sandboxes.lifecycle` from the SQLite state that sbx-mgr publishes.

## Summary

| Area | Packages | Line coverage | Branch-outcome coverage |
|---|---|---|---|
| spaced guest | `spaced-guest-server`, `-guest-client`, `-guest-svc`, `-guest-cli`, `-loopback-test` | 6,586/8,150 (80.8%) | 863/1,308 (66.0%) |
| spaced host | `spaced-host-server`, `-host-client`, `-host-svc`, `-host-cli` | 2,177/3,052 (71.3%) | 114/196 (58.2%) |
| spaced shared | `spaced-common` | 777/884 (87.9%) | 72/86 (83.7%) |
| net-gateway | `net-gateway`, `-client`, `-svc`, `net-common` | 6,957/9,680 (71.9%) | 852/1,280 (66.6%) |
| cluster-controller | `cluster-controller-server` and app, `cluster-trigger-queue`, `cluster-reconciler`, `cluster-pool` | 12,763/18,541 (68.8%) | 1,003/1,566 (64.0%) |
| internal-api | `internal-api-server` and `-svc` | 7,482/9,074 (82.5%) | 430/574 (74.9%) |
| vol-mgr | `volmgr`, `-client`, `-svc`, `-cli` | 4,697/7,189 (65.3%) | 591/956 (61.8%) |
| runtime supervisor | `node-runtime-supervisor`, `runtime-supervisor-svc` | 723/1,564 (46.2%) | 75/216 (34.7%) |
| **all 27 packages** | 3,266 functions, 702 of them never executed | **42,162/58,134 (72.5%)** | **4,000/6,182 (64.7%)** |

- **The runtime supervisor has not moved.** Its library is at exactly 723/1,554
  lines (46.5%) and 75/216 branch outcomes, the same as on 2026-10-10. Its code
  has not changed since then, and no characterization task targeted it. The
  `0a210e5` fix, "Confirm runtime termination before completing blocking
  Stop", is 1 of 79 surviving lines covered, in `service.rs` and `cgroups.rs`.
- **The weak spots are production implementations behind test fakes.** The
  reconciler tests drive a fake sink, so the Postgres implementation is never
  called:

  | Postgres implementation | Lines covered |
  |---|---|
  | `node_drain/store.rs` `SubstrateSink` | 464/1,296 (35.8%) |
  | `transient_lifecycle_sweeper/store.rs` `SubstrateSweepSink` | 85/589 (14.4%) |
  | `leader_election/store.rs` `KubeLeaseStore` | 44/165 (26.7%) |

  The Postgres writers of `sandboxes.lifecycle` in node drain
  (`restore_evacuated_and_finalize`, `mark_sandbox_lost*`,
  `abandon_stale_resident_sandboxes`, `mark_sandbox_failed_on_node`) never
  execute. Neither do the transient sweeper's undo statements.
- **Refusal paths are mostly untested.**
  - In internal-api, the quarantine compare-and-set aborts never fire:
    `record_quarantine_recovery` at `sandbox_quarantine_store.rs:1037`/`:1052`,
    the activation-rollback retire/converge at `:537`/`:575`, and the
    quarantine projection claim and compare-and-set at
    `published_state.rs:3098`/`:3130`.
  - In node-local state, every guard below is unexercised:
    - the spaced-guest process-record expiry and eviction at capacity;
    - the idempotency expiry at lookup;
    - the supervisor's superseded-execution and idempotent-rebind guards;
    - the volmgr-svc journal owner, mode and nlink refusals.
- **Most of the unreachable code sits behind a few fixed resources, each with
  a clear seam:**
  - fixed cgroup and `/proc` paths (spaced-guest `cgroup.rs`, 58/317, and the
    supervisor);
  - an env-cached credential-manager `OnceLock` (volmgr credentials and
    `execute_artifact`, about 430 lines);
  - the fixed node-manager socket (btrfs push, about 234 lines);
  - a `kubernetes()` accessor that panics on the test substrate (node drain,
    about 290 lines);
  - entrypoints that read the process environment and fixed paths
    (`run_reconcilers` 0/360, net-gateway `run` 0/271, volmgr `run` 6/212).
- **Fix churn and weak tests are not correlated.** Spearman ρ = 0.00 over 221
  files. The three files with 4 or more fix commits average 84.3% line coverage,
  against 72.5% overall. Of the lines that fix commits wrote and that survive
  at HEAD, 2,659/3,069 (86.6%) are covered.
  - **The hottest file is well covered:** spaced-guest `process.rs` has 9 fix
    commits, with 88% of its lines and 87% of its fix lines covered.
  - **The exceptions are the hot spots that matter:**

    | File | Fix commits | Line coverage | Fix-authored lines covered |
    |---|---|---|---|
    | spaced-guest `cgroup.rs` | 3 | 18% | 1/12 |
    | net-gateway `tcp_proxy/tls.rs` | 2 | 21% | 17/60 |
    | volmgr `run.rs` (7 commits in total) | 2 | 36% | 4/6 |
    | supervisor `service.rs`/`cgroups.rs` | 1 | – | 1/79 |
- **Measurement gap: `#[tonic::async_trait]` method bodies have no coverage
  mapping.** About 3,670 source lines are invisible, neither covered nor
  uncovered, mostly in:

  | File | Invisible lines |
  |---|---|
  | internal-api `adapter.rs` | 1,162 |
  | volmgr `service.rs` | 506 |
  | spaced host `filesystem.rs` | 310 |
  | spaced guest `router.rs` | 294 |
  | spaced guest `process.rs` | 288 |

  RPC-handler guards are characterized only through the inherent functions
  they call.
- **The suites are fast and stable.** The unprivileged half is 2,202 tests in
  10.3–11.8 s instrumented. The privileged half is 22 tests in 35 s. Each half
  ran 3 times with no failures and no retries. Only cluster-controller,
  `cluster-pool`, `-trigger-queue`, `-reconciler` and internal-api need
  Postgres; no package here needs Redis.

### Ranked characterization-test tasks (disjoint test files; details in Q4)

The ranking puts first the tasks that close never-exercised guards on state
writers or cover lines written by recent fixes, and orders the rest by line
gain. Gains are visible LLVM lines: the reachable adjusted-uncovered lines of
the targeted functions, discounted 15–30% for arms a fixture will not reach.
Each task owns its test file. It may add one `#[cfg(test)] mod …;` line to the
file named under "declared in", and no two tasks touch the same file.

| # | Task | Owns (new unless marked) | Declared in | Est. gain |
|---|---|---|---|---|
| 1 | cluster-controller node drain: the 18 Postgres-only `SubstrateSink` methods and their Rust refusals | `node_drain/store/sink_postgres_tests.rs` | `node_drain/store.rs` | +300 |
| 2 | cluster-controller transient sweeper: `SubstrateSweepSink` over `Substrate::Direct`, every compare-and-set and undo | `transient_lifecycle_sweeper/store/substrate_sink_tests.rs` | `transient_lifecycle_sweeper/store.rs` | +280 |
| 3 | **spaced-guest** process retention and guards (densest fix churn) | `spaced/guest/server/src/process/retention_tests.rs` | `process.rs` | +40, about a dozen guards |
| 4 | net-gateway TLS termination and handshake admission (`02d05cf`, `d11e912`) | `net/gateway/server/src/tcp_proxy/tests/tls_terminate.rs` | `tcp_proxy/tests.rs` | +180 |
| 5 | internal-api snapshot inventory and Postgres quarantine refusals, through the public API | `internal/api/server/tests/published_state.rs`, `tests/support/published_state.rs` (both existing) | – | +120 |
| 6 | volmgr job dispatch (`execute`, `execute_single`) and push guards | `vol/mgr/server/src/executor/dispatch_tests.rs` | `executor.rs` | +70 |
| 7 | spaced-host relay surface (rename, write batch, watch relays, `list_processes`, `notify_resume`) | `spaced/host/server/tests/it/relay_surface.rs` | `tests/it.rs` (host) | +300 |
| 8 | internal-api `publish_node_state` request fields | `internal/api/server/src/tests/publication/node_state.rs` | `src/tests/publication.rs` | +110 |
| 9 | internal-api data-plane route reset and runtime access | `internal/api/server/src/data_plane_sessions/tests.rs` (existing) | – | +180 |
| 10 | cluster-controller warm-pool orphan sweep with `enabled: true` | `warm_pool_orphan_sweep/sweep_tests.rs` | `warm_pool_orphan_sweep.rs` | +180 |
| 11 | net-gateway hydration and TCP-policy guards (paused time) | `net/gateway/server/src/state/hydration_tests.rs` | `state.rs` | +110 |
| 12 | volmgr push submission and job RPC helpers | `vol/mgr/server/src/service/push_submit_tests.rs` | `service.rs` | +120 |
| 13 | spaced-guest node-local guards: idempotency expiry, ledger overflow, port-forward | `idempotency/expiry_tests.rs`, `filesystem/watch/ledger/overflow_tests.rs`, `port_forward/guard_tests.rs` | their parents | +40, about 10 guards |
| 14 | runtime supervisor execution and inventory guards | `supervisor/src/service/execution_guard_tests.rs`, `src/process/readonly_tests.rs`, `src/inventory/readonly_tests.rs` | `service.rs`, `process.rs`, `inventory.rs` | +85 |
| 15 | internal-api receipt and batch validation | `internal/api/server/src/published_state_tests.rs` (existing) | – | +55 |
| 16 | cluster-controller `KubeLeaseStore` against a `service_fn` apiserver | `leader_election/store/kube_lease_store_tests.rs` | `leader_election/store.rs` | +90 |
| 17 | spaced-guest client and guest networking RPCs | `spaced/guest/server/tests/it/client_and_networking.rs` | `tests/it.rs` (guest) | +90 |
| 18 | cluster-reconciler trigger fanout loop | `cluster/reconciler/src/trigger_fanout/loop_tests.rs` | `trigger_fanout.rs` | +60 |
| 19 | volmgr-client replica-provenance round trip | `vol/mgr/client/src/provenance_tests.rs` | `lib.rs` | +35 |
| 20 | trigger-queue claim-generation compare-and-set | `cluster/trigger-queue/src/claim_generation_tests.rs` | `lib.rs` | +12 and 3 guards |

Together the tasks add about 2,450 visible lines, taking the 27 packages from
72.5% to about 77%. The seams in Q3 unlock about 2,900 more.

## Method

| Step | Detail |
|---|---|
| Environment | The CI image, built locally from `ci/Dockerfile.ci` with `SPACE_CI_UID` set to the local uid. It provides rust 1.98.0, `cargo-nextest 0.9.133` and `protoc 35.0`. Added on top: `cargo-llvm-cov 0.9.1`, 1.98.0 `llvm-tools`, and nightly `llvm-tools` (rustc 1.101.0-nightly 2026-10-11, LLVM 23.1.3). The published image tag needs registry credentials, so it was not pulled. |
| Instrumentation | `cargo llvm-cov show-env --branch`, with `RUSTC_BOOTSTRAP=1` on the pinned compiler, as on 2026-10-10. |
| Packages | The 27 packages in the Summary table, all prefixed `pplx-space-`. |
| Unprivileged half | `cargo nextest run --locked --all-targets --all-features -p … --run-ignored all --ignore-default-filter --retries 1 --no-fail-fast --test-threads 30 -E 'not group(privileged)'`. It ran as uid 1000 with `--cap-drop=ALL --security-opt=no-new-privileges`, no Docker socket and no host network. Postgres and Redis came from `ci/scripts/ci-test-services.sh` on a bridge network, passed through `SPACE_NEXTEST_POSTGRES_ADDRESS`/`_REDIS_ADDRESS` as in `common-ci.yml`. `SPACE_CI_REQUIRE_TEST_DEPS=1` was set. Result: 2,202 passed, 22 skipped (the privileged group), 0 `SPACE-TEST-DEP-SKIP` records. |
| Privileged half | The same command with `-E 'group(privileged)'`, as root in a `--privileged` container with the Docker socket and no host network, reusing the same build. 22 tests passed: 16 `volmgr-svc` `identity_handoff`, 4 `spaced-guest-server` (`worker_subprocess`, `worker_telemetry` and two `filesystem::router` descriptor reads), 1 `net-gateway` and 1 `net-gateway-client`. The target directory was made world-writable for this step, so that children which switch uid can still write profiles. |
| Reporting | `cargo llvm-cov report --branch --json`, with `LLVM_COV` set to the nightly `llvm-cov` and `LLVM_PROFDATA` left on 1.98.0's. This is the 2026-10-10 workaround: the pinned LLVM 22.1.8 `llvm-cov export` segfaults on these binaries. |
| Line and branch rule | LLVM's own line rule (`LineCoverageStats`), recomputed from the exported segments. Branch outcomes are the true/false outcomes of the conditions rustc instruments (`if`, `while`, `let … else`, `&&`/`||`, `if let`). Each is counted once per source location: generic instantiations are merged, and an outcome is covered if any instantiation took it. `?` and plain `match` arms are not branches. |
| Functions | Source `fn` items, found with `syn`. Closures and `async` bodies roll into their enclosing `fn`. "Adjusted uncovered" excludes lines that are only punctuation or `?`. |
| Excluded as test code | `tests.rs`, `*_tests.rs`, `tests/` and `test_support` files; files declared by a `#[cfg(test)] mod x;`; and items whose `cfg` mentions `test` other than as `not(test)`. 23 files have no code on Linux (`cfg(not(unix))`, non-Linux unix, or declaration-only `mod.rs`) and do not appear. |
| Cross-package execution | A package's coverage is the union over all 27 packages' tests. For example, `spaced-host-server` tests drive `spaced-host-client`. Packages outside the set were not run: `pplx-space-api`'s tests may reach cluster-pool store verbs and `cluster-trigger-queue` enqueue functions that show 0% here. |
| Privileged contribution | Re-exporting without the privileged profiles removes 693 lines from `volmgr-svc` (`identity_handoff` 539 → 0), 249 from `spaced-guest-server` (`cgroup.rs` 58 → 4, `filesystem/worker/linux.rs` 462 → 326, `router.rs` 567 → 527), 48 from `net-gateway`, 30 from `net-gateway-client` and 3 from `net-common`. |
| Lost child profiles | `filesystem/worker/linux.rs:751` re-executes the identity worker with `env_clear()`, which drops `LLVM_PROFILE_FILE`. The privileged run left 5 `default_*.profraw` files in the crate directory; folded back in, they add 4 lines. The worker's own loop (`run_worker` 0/22, `serve_worker_stream` 0/23) still shows 0, because its profile is lost. `pre_exec` closures run in the forked child before `exec`, so no test can measure them. |
| Check against the prior baseline | The supervisor reproduces the 2026-10-10 numbers exactly. |
| Flakes and timing | Each half ran 3 times: the measured run plus two reruns with discarded profiles, each against freshly started test services. All 2,224 tests passed every time, with no retries and no SLOW, TIMEOUT or LEAK status. The longest test is `identity_handoff::tests::killed_handoffs_resume_or_reverse_without_losing_data`, at 33.6 s. |
| Fix churn | `git log --since=2026-09-10 79b1d46..HEAD` over the package directories gives 57 commits after the 2026-10-05 public import; the import is excluded because it touches every file. 31 of them are fixes by subject and diff (listed in Q5). "Fix-authored lines" are the lines `git blame HEAD` attributes to a fix commit. |
| Code reading | Guard and seam findings come from reading each function next to its per-line counts. Every line cited as never executed is `####` (mapped, 0 executions) or a 0-count branch outcome. |

**Limits.**
- This is unit and integration coverage only. Kind, smoke and Gateway traffic
  exercise netns, nft, Firecracker, vsock, real btrfs and S3 paths, and were
  not measured. Doctests were not measured either.
- There is no per-test attribution, and no refusal-branch probe build this
  time. A refusal written as `Ok(rows == 1)` has no branch region, so it is
  marked *invisible* unless a test asserts it.
- `#[tonic::async_trait]` bodies are unmapped (see the Summary).
- Log-field expressions inside `tracing` macros show as never executed,
  because the tests install no subscriber.

## Q1 — Line coverage per crate, per file, and the lowest-covered lifecycle functions

### Per package

| Package | Tests (privileged) | Functions | Never executed | Lines | Branch outcomes |
|---|---|---|---|---|---|
| `spaced-guest-server` | 256 (4) | 527 | 47 | 6,586/7,890 (83.5%) | 863/1,284 (67.2%) |
| `spaced-guest-client` | 0 | 11 | 11 | 0/95 (0%) | – |
| `spaced-guest-svc` / `-guest-cli` / `-loopback-test` | 0 | 10 | 10 | 0/165 (0%) | 0/24 |
| `spaced-host-server` | 66 | 141 | 22 | 1,540/1,945 (79.2%) | 107/162 (66.0%) |
| `spaced-host-client` | 3 | 47 | 12 | 601/884 (68.0%) | 2/8 |
| `spaced-host-svc` / `-host-cli` | 1 | 12 | 9 | 36/223 (16.1%) | 5/26 |
| `spaced-common` | 37 | 94 | 9 | 777/884 (87.9%) | 72/86 (83.7%) |
| `net-gateway` | 240 (1) | 575 | 105 | 6,051/8,517 (71.0%) | 753/1,142 (65.9%) |
| `net-gateway-client` | 1 (1) | 16 | 11 | 47/102 (46.1%) | – |
| `net-gateway-svc` | 1 | 2 | 1 | 9/27 (33.3%) | – |
| `net-common` | 31 | 109 | 24 | 850/1,034 (82.2%) | 99/138 (71.7%) |
| `cluster-controller-server` | 934 | 795 | 243 | 10,696/15,767 (67.8%) | 947/1,480 (64.0%) |
| `cluster-controller` (app) | 0 | 2 | 2 | 0/17 (0%) | – |
| `cluster-trigger-queue` | 11 | 13 | 1 | 393/412 (95.4%) | 15/22 |
| `cluster-reconciler` | 12 | 26 | 9 | 283/544 (52.0%) | 14/30 |
| `cluster-pool` | 66 | 95 | 16 | 1,391/1,801 (77.2%) | 27/34 |
| `internal-api-server` | 308 | 334 | 50 | 7,482/9,059 (82.6%) | 430/574 (74.9%) |
| `internal-api-svc` | 0 | 2 | 2 | 0/15 (0%) | – |
| `volmgr` | 199 | 299 | 63 | 3,794/5,751 (66.0%) | 452/754 (59.9%) |
| `volmgr-client` | 15 | 15 | 9 | 129/260 (49.6%) | 9/16 |
| `volmgr-svc` | 20 (16) | 43 | 2 | 774/810 (95.6%) | 130/168 (77.4%) |
| `volmgr-cli` | 0 | 7 | 7 | 0/368 (0%) | 0/18 |
| `node-runtime-supervisor` | 23 | 89 | 35 | 723/1,554 (46.5%) | 75/216 (34.7%) |
| `runtime-supervisor-svc` | 0 | 2 | 2 | 0/10 (0%) | – |

### Per file

Files with 40 or more mapped lines and under 70% line coverage, lowest first
(`also` lists larger files above 70%). The CSV `(file)` rows have every file.

| Package | Files under 70% | Also |
|---|---|---|
| spaced-guest-server | `cgroup.rs` 58/317 (18%), `memlock.rs` 28/60 (47%), `identity/linux.rs` 46/85 (54%) | `process.rs` 1,027/1,167 (88%), `filesystem/worker/linux.rs` 462/577 (80%), `lib.rs` 353/440 (80%), `filesystem/router.rs` 567/652 (87%), `filesystem/watch/backend.rs` 633/786 (81%), `idempotency.rs` 271/295 (92%) |
| spaced-host-server, -client, guest-client, common | host `filesystem.rs` 92/180 (51%), host `run.rs` 58/99 (59%), host `process.rs` 65/99 (66%), host-client `lib.rs` 601/884 (68%), guest-client `lib.rs` 0/95, common `vsock.rs` 75/112 (67%) | host `bridge.rs` 836/997 (84%) |
| net-gateway, -client, net-common | `run.rs` 53/479 (11%), `tcp_proxy/tls.rs` 118/562 (21%), `control.rs` 57/102 (56%), `udp_proxy.rs` 543/949 (57%), `ingress/mod.rs` 390/628 (62%), client `lib.rs` 47/102 (46%), common `orig_dst/linux.rs` 9/71 (13%) | `state.rs` 601/787 (76%), `reserved_dns.rs` 345/465 (74%), `tcp_proxy.rs` 362/476 (76%), `tls_http/connect.rs` 117/168 (70%) |
| cluster-controller-server | never executed: `lib.rs` 0/62, `kube_node_sync.rs` 0/50, `orphaned_restore_pod_gc/{backend,config}.rs` 0/46, 0/59, `stuck_container_creating_gc/backend.rs` 0/70, `snapshot_storage_sweeper/store.rs` 0/54; then `reconcilers.rs` 29/426 (7%), `node_drain/aws.rs` 6/82 (7%), `data_plane_session_retention.rs` 13/94 (14%), `transient_lifecycle_sweeper/store.rs` 85/589 (14%), `orphaned_sandboxes/quarantine_recovery.rs` 16/94 (17%), `node_group_mirror.rs` 21/123 (17%), `warm_pool_orphan_sweep.rs` 93/383 (24%), `leader_election/store.rs` 44/165 (27%), `node_service_readiness/backend.rs` 46/134 (34%), `node_drain/store.rs` 464/1,296 (36%), `public_operation_retention.rs` 26/70 (37%), `stuck_snapshot_reaper/mod.rs` 56/101 (55%), `node_group_labeler/{mod,kube}.rs` 52/90, 59/98, `suspended_execution_reaper/mod.rs` 212/335 (63%), `failed_restorable_repair/mod.rs` 87/136 (64%), `admin_audit_retention.rs` 105/153 (69%) | `node_drain/mod.rs` 740/828 (89%), `transient_lifecycle_sweeper/mod.rs` 1,096/1,296 (85%), `orphaned_sandboxes.rs` 813/987 (82%), `public_operation_dispatcher.rs` 374/461 (81%), `warm_pool.rs` 1,192/1,648 (72%) |
| cluster-reconciler, -pool, -trigger-queue | `trigger_fanout.rs` 182/370 (49%), `metrics.rs` 101/174 (58%) | pool `store.rs` 1,192/1,577 (76%), trigger-queue `lib.rs` 393/412 (95%) |
| internal-api-server | `run.rs` 34/156 (22%), `adapter.rs` 184/274 (67%; most of its 1,696 lines are unmapped `async_trait` bodies) | `metadata.rs` 563/810 (70%), `data_plane_sessions.rs` 1,091/1,436 (76%), `published_state.rs` 2,596/2,992 (87%), `sandbox_quarantine_store.rs` 481/533 (90%), `auto_lifecycle_delete_completions.rs` 383/416 (92%), `node_local_backup_pending.rs` 197/201 (98%) |
| volmgr, -client, -cli, -svc | `credentials.rs` 5/106 (5%), `executor/stream_push/btrfs.rs` 37/203 (18%), `run.rs` 134/370 (36%), `btrfs_push.rs` 74/166 (45%), `service.rs` 553/922 (60%, plus 506 unmapped `async_trait` lines), `staging.rs` 66/104 (63%), client `lib.rs` 129/260 (50%), cli `main.rs` 0/368 | `executor.rs` 2,587/3,509 (74%), `executor/stream_push.rs` 209/229 (91%), `db.rs` 80/87 (92%); every volmgr-svc file is above 80% (`identity_handoff.rs` 539/554) |
| node-runtime-supervisor | `os/linux.rs` 0/21, `cgroups.rs` 5/81 (6%), `run.rs` 5/43 (12%), `inventory.rs` 18/115 (16%), `process.rs` 25/79 (32%), `service.rs` 303/714 (42%), `sidecar_launcher.rs` 97/195 (50%) | `service/execution.rs` 105/132 (80%), `execution_store.rs` 165/174 (95%) |

### Lowest-covered functions on lifecycle paths

The "Blocker" column says why the function is not reached. "seam" means
production code needs a test hook (see Q3). "no test" means a unit test can
reach it today.

| Function | file:line | Lines | Branch outcomes | Blocker |
|---|---|---|---|---|
| `run_reconcilers` | cluster-controller `reconcilers.rs:189` | 0/360 | 0/38 | seam: env `from_env`, AWS SDK, spawns |
| `SubstrateSink::restore_evacuated_and_finalize` | cluster-controller `node_drain/store.rs:1649` | 0/106 | 0/8 | seam: `kubernetes()` panics on the test substrate |
| `SubstrateSink::promote_local_snapshot` | `node_drain/store.rs:2041` | 0/87 | 0/12 | same, plus a node-API client |
| `SubstrateSink::abandon_stale_resident_sandboxes` | `node_drain/store.rs:2189` | 0/65 | 0/2 | same |
| `SubstrateSink::mark_sandbox_failed_on_node` | `node_drain/store.rs:2597` | 0/58 | 0/2 | same |
| `SubstrateSink::mark_sandbox_lost` (and 17 other Postgres-only sink methods, about 352 lines) | `node_drain/store.rs:2449` | 0/34 | 0/2 | no test: only `FakeSink` is driven |
| `SubstrateSink::evacuate_sandbox` | `node_drain/store.rs:2744` | 63/107 | 5/10 | the fake suspend only fails, so the commit at `:2914` never runs |
| `SubstrateSweepSink::list_aged_transient` | `transient_lifecycle_sweeper/store.rs:783` | 0/55 | – | no test: fake sink only |
| `SubstrateSweepSink::list_failed_active_executions` | `transient_lifecycle_sweeper/store.rs:1156` | 0/41 | – | same; its test runs a pasted copy of the SQL |
| `sweep` / `act_on_orphan` | cluster-controller `warm_pool_orphan_sweep.rs:306` / `:485` | 0/125, 0/74 | 0/24, 0/6 | no test: `SweepConfig::default()` has `enabled: false` |
| `Repair::recover` | `orphaned_sandboxes/quarantine_recovery.rs:70` | 0/31 | 0/6 | seam: concrete `Arc<KubernetesSubstrate>` |
| `KubeLeaseStore::acquire_or_renew` | `leader_election/store.rs:266` | 0/46 | 0/6 | no test: a `service_fn` kube client suffices |
| `drain_ready_triggers` / `process_claim` | cluster-reconciler `trigger_fanout.rs:256` / `:290` | 0/27, 91/133 | 0/2, 5/8 | no test: the endpoint resolves to an empty string |
| `publish_sandbox_snapshots_batch` | internal-api `published_state.rs:3784` | 41/142 | 3/4 | no test fills `PublishedSandboxState.snapshots` |
| `prune_absent_node_templates` | internal-api `published_state.rs:3696` | 0/48 | – | `template_inventory_complete` is never true |
| `parse_published_transition_receipt` | internal-api `published_state.rs:87` | 71/125 | 9/12 | no malformed-receipt cases |
| `publish_sandbox_states` | internal-api `published_state.rs:1698` | 1,195/1,260 | 37/46 | whole-batch rejections at `:1884`–`:1927` |
| `reset_sandbox_service_route` | internal-api `data_plane_sessions.rs:657` | 0/80 | 0/4 | callers live in node-ctl and data-plane |
| `published_node_status_db` | internal-api `lib.rs:249` | 0/24 | – | no test sends `node_status` |
| `TransferExecutor::execute_artifact` | volmgr `executor.rs:4178` | 0/178 | 0/16 | seam: credential-manager `OnceLock` |
| `TransferExecutor::execute_single` | volmgr `executor.rs:4058` | 0/73 | – | no test: dispatch `execute` (`:3896`) is never called |
| `TransferExecutor::execute_btrfs_push` | volmgr `executor/stream_push/btrfs.rs:73` | 0/165 | 0/20 | seam: fixed node-manager socket |
| `TransferExecutor::execute_pull_locked` | volmgr `executor.rs:5106` | 328/452 | 48/80 | encrypted and skip-present arms |
| `TransferExecutor::execute_push_inner` | volmgr `executor.rs:4620` | 86/159 | 14/24 | encrypted arm needs the credential seam |
| `register_push_job` | volmgr `service.rs:698` | 0/53 | 0/14 | no test submits a push |
| `run` | volmgr `run.rs:411` | 6/212 | 0/28 | seam: fixed `SERVICE_MUTEX_PATH` |
| `TcpProxy::terminate_tls` / `admit_handshake` | net-gateway `tcp_proxy/tls.rs:386` / `:622` | 0/56, 0/23 | 0/6 | no test: the interception tests use a failing signer |
| `open_service_port_ingress` | net-gateway `ingress/mod.rs:453` | 0/71 | 0/4 | seam: fixed guest address `GUEST_IPV4_ADDR` |
| `GatewayNetworkState::run_hydration_loop` | net-gateway `state.rs:540` | 19/62 | 3/14 | no test: retry exhaustion and timeout |
| `UdpProxy::get_or_create_session` | net-gateway `udp_proxy.rs:913` | 15/94 | 1/8 | `SO_MARK` / `IP_TRANSPARENT` |
| `linux::initialize_at` / `linux::cap_user_slices` | spaced-guest `cgroup.rs:405` / `:526` | 0/66, 0/32 | 0/18, 0/8 | seam: fixed `/sys/fs/cgroup`; the test host is the cgroup root |
| `lock_read_only_file_mappings` | spaced-guest `memlock.rs:45` | 0/22 | 0/2 | runs only when `/dev/vsock` exists; `mlock2` is process-wide |
| `run_worker` / `serve_worker_stream` | spaced-guest `filesystem/worker/linux.rs:875` / `:909` | 0/22, 0/23 | – | run in the worker child, whose profile is lost |
| `SpacedGuestService::run_bridge_guest_server` | spaced-guest `lib.rs:754` | 32/65 | 3/4 | vsock listener arm |
| `ProcessLaunch::publish` | spaced-guest `process.rs:322` | 121/139 | 14/16 | failed-start cleanup (`d5bc2db`, `026e103`) |
| `ProcessRegistry::reserve` / `ensure_reaper` | spaced-guest `process.rs:1362` / `:1436` | 19/28, 15/21 | 1/2, 3/8 | eviction at capacity; reaper idle exit (`654d405`) |
| `SpacedHostService::call_bridge_guest_rename` / `notify_resume` | spaced-host `bridge.rs:1207` / `:979` | 0/39, 0/26 | – | no test (the `MockBridgeGuest` already implements both) |
| `SpacedHostClient::write_batch_as_user` | spaced-host-client `lib.rs:622` | 0/36 | – | no caller in tests |
| `launch_sidecar_owned` / `launch_firecracker_owned` | supervisor `service.rs:678` / `:546` | 0/128, 0/104 | 0/18, 0/18 | seam: the peer authorizer needs a real kernel pid |
| `stop_runtime_processes` / `terminate_sandbox_cgroups` | supervisor `service.rs:917` / `cgroups.rs:48` | 0/35, 0/43 | 0/8, 0/6 | seam: fixed cgroup root; these carry the `0a210e5` fix |
| `live_sidecar_pid_from_cgroup` | supervisor `inventory.rs:60` | 0/33 | 0/6 | seam: fixed `/proc` and run directory |

## Q2 — State and lifecycle writers whose refusal or guard paths never run

How to read this section:
- *Never* means the guard is `####` or its outcome count is 0.
- *Invisible* means a SQL predicate whose 0-row result has no Rust branch, or
  that no test asserts.
- Line numbers are source lines.

### Postgres writers: internal-api

| Writer | Writes | Refusal or guard never exercised |
|---|---|---|
| `record_quarantine_recovery` `sandbox_quarantine_store.rs:999` | INSERT Postgres `sandbox_quarantine_history`; DELETE Postgres `sandbox_quarantine_state` | `inserted != 1` (`:1037`, T0/F5); `removed != 1` (`:1052`, T0/F5) |
| `apply_activation_rollback_candidates` `:502` | Postgres `sandbox_k8s_executions`, `sandbox_port_exposures`, Postgres `sandboxes.lifecycle`/`placement`/node ids | `retired == 0` (`:537`, T0/F6); `converged != 1` (`:575`, T0/F6). Both repeat an earlier `FOR UPDATE` predicate, so they need a concurrent writer. |
| `recover_pause_failed_from_inventory` `:586` | Postgres `sandboxes.lifecycle` quarantined → started/paused; quarantine state and history | unsupported target (`:638`); compare-and-set lost (`:711`, T0/F2) |
| Quarantine projection in `publish_sandbox_states` `published_state.rs:3040`–`3202` | Postgres `sandboxes.lifecycle` → quarantined; upserts Postgres `sandbox_quarantine_state`; inserts history | claim refused (`:3098`, T16/F0); compare-and-set lost on `s.lifecycle = input.from_lifecycle` (`:3130`, T16/F0). The claim `SELECT` takes no `FOR UPDATE`. |
| Suspended finalization `published_state.rs:3519` | Postgres `sandbox_k8s_executions`, `sandboxes` suspended columns, warm-pool slots | recovery with no projection (`:3425`); ready-suspend compare-and-set lost (`:3502`, T3/F0); empty early-out (`:3523`) |
| Absent-stopped delete `published_state.rs:2181` | DELETE Postgres `sandboxes` (`active_node_id`, `managed_by='space'`, `lifecycle='stopped'`) | invisible: no test asserts that a non-matching row survives |
| `prune_absent_node_sandboxes` `status.rs:6` | Postgres `sandboxes.lifecycle='deleted'`, clears node ids and leases | invisible: only positive assertions |
| `clear_sandbox_command_ready_pending` `data_plane_sessions.rs:1310` | Postgres `sandboxes.command_ready_pending` | started pre-check (`:1248`, T0/F2); lost race (`:1285`, T0/F2) |
| `finalize_backup_attempt` `lib.rs:583` | Postgres `snapshots.state`/`size_bytes` (compare-and-set), then suspend finalization | row vanished (`:655`); source-node mismatch after the compare-and-set (`:666`, T0/F34). The compare-and-set refusal itself is exercised once (`:643`). |
| `bootstrap_k8s_origin_node` `lib.rs:1316` | Postgres `nodes`, `node_k8s` | conflict (`:1372`, T0/F1); tombstone (`:1395`, T0/F1) |
| `publish_node_state` `publish.rs:10` | node status and inventories | control-plane role publishing sandbox inventory (`:28`–`:30`) |

These internal-api paths are covered:
- Activation receipts: `consume_activation_completion_receipts` (`sandbox_quarantine_store.rs:341`) and its generation, attempt and target fence.
- Postgres `node_local_backup_pending` reconcile and clear (98%).
- `apply_receipt` for auto-lifecycle delete completions. Every branch is taken both ways, except the non-lock database errors and an unreachable arm at `:710`.

internal-api never stores receipts itself. They arrive inside node publications.

### Postgres writers: cluster-controller, pool and trigger queue

| Writer | Writes | Refusal or guard never exercised |
|---|---|---|
| `evacuate_sandbox` commit `node_drain/store.rs:2914` | Postgres `sandboxes.lifecycle` suspending → suspended | The whole block is never executed. `rows_affected` is not checked, so a lost compare-and-set would still emit `SuspendCompleted`. This is an observation for follow-up, not a confirmed defect. |
| `evacuate_sandbox` pre-check `:2788`; revert `:152` | Postgres `sandboxes.lifecycle`, `.evacuation_restore_target` | non-evacuable lifecycle (`:2788`, T0/F3); successful revert (`:2880`); database-error arm (`:2865`) |
| `restore_evacuated_and_finalize` `:1649` | Postgres `sandboxes.lifecycle`/`placement`; `node_reconcile_queue` | eligibility (`:1700`); claim lost (`:1752`); `rows_affected() == 0` (`:1813`) |
| `mark_sandbox_lost{,_on_gone_node}`, `make_sandbox_durable_*`, `fail_inflight_snapshots_on_gone_node` | Postgres `sandboxes.lifecycle='failed'`, local-artifact columns; Postgres `snapshots.state` | the Rust `flipped` checks at `:2486`, `:2532`, `:2580`, `:2726`. The SQL predicates run only through the query-builder tests. |
| `abandon_stale_resident_sandboxes`, `mark_sandbox_failed_on_node` | Postgres `sandboxes.lifecycle='failed'` | empty claim (`:2290`); `let … else` (`:2628`); `SKIP LOCKED` contention (invisible) |
| `delete_terminating_node_and_record_tombstone` `:3476` | DELETE Postgres `nodes`; UPSERT `node_k8s_tombstones` | `rows_affected() == 0` (`:3504`, T0/F1) |
| Transient sweeper `converge_restoring` / `revert_to_suspended` | Postgres `sandboxes.lifecycle` | reconciler compare-and-set lost (`mod.rs:1352`, T0/F2; `:1546`, T6/F0) |
| Transient sweeper undo statements `store.rs:1017`/`1076`/`1326` | Postgres `sandboxes.lifecycle`, `.command_ready_pending` | `undo_fail_aged_candidate_query` and `undo_reap_command_ready_pending_query` have never run against Postgres. The undo-refused arms (`mod.rs:1664`, `:1885`, `:2283`) are never taken, because the fake always returns `Ok(true)`. |
| `quarantine_orphan` `orphaned_sandboxes.rs:1613` | Postgres `sandboxes.lifecycle='quarantined'`; Postgres `sandbox_quarantine_state`/`_history` | compare-and-set refusal (`:1233`, T3/F0); the Forgotten-guard SQL (`:1551`, `:1692`); rate-window expiry (`:619`) |
| `recover_orphan_to_suspended` `orphaned_sandboxes.rs:1560` | Postgres `sandboxes` lifecycle, placement and node ids; executions | compare-and-set `None` (`:1195`, T3/F0); re-plan `None` before `FinalizeDelete` (`:1166`) |
| Quarantine recovery `quarantine_recovery/store.rs:93` | Postgres `sandboxes.lifecycle='suspended'`; deletes quarantine and observed state | the dry-run and false-return arms of the caller (`quarantine_recovery.rs:87`, `:89`); the evidence predicates are invisible |
| `retire_stale_execution` (suspended reaper) | Postgres `sandbox_k8s_executions`, `sandboxes` placement and artifact columns | `SkippedSnapshotNotCurrent` and Err arms in the reconciler (`mod.rs:699`, `:715`) |
| Public-operation dispatcher | Postgres `public_operations` | claim-token fence (`:809`, T0/F5); lost renewal (`:584`, T0/F1) |
| Pool `delete_locked_*`, `fence_ready_slot`, `release_claimed_slot`, `invalidate_claimed_slot`, `disable_config` | Postgres `kubernetes_pool_slots`, `kubernetes_pool_configs` | the reconciler lock refusals (`warm_pool.rs:1868`, `:1964`, `:1997`). The verbs whose callers are in `pplx-space-api` show 0% here (not measured). |
| Trigger queue `complete_claim` / `dead_letter_claim` | Postgres `node_reconcile_triggers` | claim-generation compare-and-set (`:295`, `:343`, `:400`) invisible: no test re-enqueues between claim and complete |
| `delete_storage_location_if_orphan` `snapshot_storage_sweeper/store.rs:110` | DELETE Postgres `snapshot_storage_locations` | the whole function never runs; only the fake does |

There is no per-statement leader fence in cluster-controller. Leadership acts
only by cancelling the reconcilers' token (`leader_election/mod.rs:157`–`165`).
`KubeLeaseStore` (a Kubernetes Lease, not Postgres) never runs its 409 and
"takeable" refusals (`store.rs:290`, `:309`, `:318`, `:335`).

### Node-local state (no sbx-mgr SQLite here)

| Writer | State | Refusal or guard never exercised |
|---|---|---|
| volmgr `upsert_attachment` / `delete_attachment` `db.rs:41`/`:69` | volmgr SQLite `volume_attachments` | Only the SQL-error tails (`:65`, `:77`). The upsert-on-conflict and repeated delete paths are exercised. |
| volmgr `attach_volume` / `detach_volume` RPCs `service.rs:1019`/`:1046` | volmgr SQLite `volume_attachments` | missing spec (`:1029`); bad `sandbox_id` (`:1034`, `:1057`). The handler body is unmapped (`async_trait`), and no test attaches successfully through the RPC. |
| volmgr `register_push_job`, `cancel_transfer`, `submit_pull`, `authorize_job` | `common-jobs` rows and staged files | submit-error unlink (`service.rs:754`–`765`); dedup-hit orphan cleanup (`:774`–`787`); cancel not-found, not-a-pull and scope parse (`:1372`–`1390`); transfer-scope dedup key (`:883`); root bypass and missing subject (`:310`–`332`) |
| volmgr-svc `Journal::read`/`open`/`write` `identity_handoff/journal.rs` | root-owned handoff journal | owner/mode/nlink refusals (`:118`, T724/F0; `:161`; `:184`–`187`); non-NotFound I/O errors (`:133`, `:168`, `:200`, `:212`); rename failure (`:294`) |
| volmgr-svc `State::finish`, `prepare_inner`, `restore_prepared` | ownership transfer phases | sealed directory (`identity_handoff.rs:524`), shared runtime directory (`:657`), run-parent ownership (`:686`), cache-ancestor uid (`:707`), node root (`:272`), inode match on restore (`:462`), expected-identity mismatch (`:770`) |
| supervisor `bind_execution_owned` / `retire_execution_owned` `service/execution.rs:90`/`:121` | on-node execution records | idempotent rebind (`:108`, T0/F1); superseded bind (`:111`) and superseded retire (`:138`); quiescence (`:38`); absent binding allowed (`:78`) |
| supervisor `ExecutionStore::remove` / `cpu_budget` `execution_store.rs:215`/`:122` | records and `.cpu` budget files | absent directory (`:217`, `:128`); open and remove errors (`:137`, `:225`, `:231`); nlink and is-file checks (`:140`) |
| supervisor `update_cgroup_owned` `service.rs:956`; `write_pid_symlink` `inventory.rs:103` | cgroup quota; pid symlink | non-increasing refusal (`:999`) and the success write (`:1026`); foreign-pid refusal (`inventory.rs:110`) |
| spaced-guest `ProcessRegistry::claim`/`reserve`/`get_*`/`list` `process.rs:1339`–`1503` | in-guest process records | duplicate id after Published (`:1343`); eviction of completed records at capacity (`:1384`–`1396`); closed registry (`:1373`); expired lookups (`:1406`, `:1496`, `:1503`); `get_running` on an exited process (`:1415`); reaper wake and idle exit (`:1449`–`1457`) |
| spaced-guest signal, input, close-stdin and resize RPCs `process.rs:2236`–`2335` | process I/O | unknown id (`:2249`, `:2281`, `:2303`, `:2327`); missing PTY size (`:2322`); unpublished claim drop (`:595`) |
| spaced-guest `ProcessEventJournal::try_append_output` `process.rs:1102` | output ring and shared slots | shared slots exhausted with an empty ring (`:1171`, `:1181`–`1184`). Backpressure and oversized chunks are exercised. |
| spaced-guest `IdempotencyStore` `idempotency.rs:56`/`:118`/`:252` | idempotency key table | completed record expired at lookup (`:139`–`146`); in-progress record expired (`:152`–`161`); poison recovery (`:58`–`62`); abandon fallthrough (`:263`) |
| spaced-guest `require_ready`, `call_bridge_host_response` `lib.rs:472`/`:650` | readiness gate; bridge-host call slots | shutting down (`:474`); not ready (`:479`); slot refusal (`:665`) |
| spaced-guest `LoopbackBridgeManager::open`/`close` `port_forward/mod.rs:319`/`:445` | port-forward table | cancelled before open or under the lock (`:325`, `:351`); port 0 (`:331`); closed channel (`:402`); close of an unknown port (`:456`); join timeout (`:453`) |
| spaced-guest `WorkerPool::client_for`/`spawn_worker` `filesystem/worker/linux.rs:308`/`:589` | identity workers | idle-LRU eviction (`:340`); shutting down (`:591`); slot refusal (`:598`); connect error and startup timeout (`:679`–`705`) |
| spaced-guest `Ledger::enforce_capacity` `filesystem/watch/ledger.rs:241`; `run_pull_reaper` `watch.rs:865` | file-change ledger; pull watchers | event-count overflow (`:244`–`250`); expiry tick (`watch.rs:871`–`883`) |
| spaced-guest cgroup writers `cgroup.rs:405`/`:691`/`:702` | `/sys/fs/cgroup` | everything except the `CgroupRoot` refusal (`:630`) |
| spaced-host `check_guest_signal_rate` `bridge.rs:410` | signal arrival window | window expiry (`:420`); poisoned lock (`:413`) |
| net-gateway `GatewayNetworkState` `state.rs:540`–`1082` | per-sandbox policy and mode | retry-budget exhaustion (`:590`); `PolicyRefMismatch` recheck (`:567`); invalid pulled reference (`:864`); hydration timeout (`:911`); TCP-policy wait timeout and Draining/Stopped denial (`:973`–`1036`); stopped with a policy installed (`:1082`) |
| net-gateway `ServicePortIngressState::update_identity` `ingress/mod.rs:126` | route identity and epoch | trusted-sandbox mismatch (`:135`); missing generation on an existing identity (`:153`); sandbox_id or substrate conflict (`:295`, `:307`) |
| net-gateway certificate cache `certificates/client.rs:151`, `authority.rs:509` | leaf LRU; CA | pending-full (`:212`); not admissible (`:239`); signing error (`:340`); expired CA, epoch cancel and unusable CA (`authority.rs:525`–`618`) |

## Q3 — Code that needs a seam, and the seam that unlocks it

| Blocker (where) | Unlocks | Minimal seam |
|---|---|---|
| `kubernetes()` panics on the `SuspendOnly` test substrate (`node_drain/store.rs:1488`; callers `:1731`, `:2329`, `:2659`, `:3311`) | about 290 lines of Postgres `sandboxes.lifecycle` writers in node drain | Test closures for delete, restore and schedulability on `SuspendOnly`, plus an injectable node-API client factory (`:1628`). The 18 Postgres-only sink methods need no seam. |
| volmgr credentials: `connect_from_env()` reads an env-cached `OnceLock` endpoint (`credentials.rs:63`, `:166`, `:208`) | about 430: `credentials.rs`, `execute_artifact`, and the encrypted push and pull arms | A `CredentialSource` trait on `TransferExecutor` and `VolumeManagerService`; the test implementation returns static credentials and a DEK |
| Fixed node-manager socket: `connect_once()` and `lazy()` `OnceLock` (`btrfs_push.rs:34`, `:51`, `:173`; `executor.rs:3921`) | about 234 (btrfs push and orphan cleanup) | Pass a client in the way `with_node_manager` does (`executor.rs:3537`), served by a fake `NodeManager` over a tempdir UDS |
| spaced-guest `CGROUP_ROOT` `/sys/fs/cgroup` and `/proc/self/cgroup`, `/proc/sys/kernel/*`, `/proc/meminfo` (`cgroup.rs:273`, `:621`, `:713`, `:725`) | about 200 (`initialize_at`, `cap_user_slices`, io weight, `delegated_root`, `memory_max`). This is the `532113a` envelope. | A `CgroupPaths` struct passed to `initialize_at`; a tempdir works because cgroup files are plain writes. Also a command-runner closure for `systemctl set-property` (`:582`, about 28 lines). |
| Supervisor peer authorizer needs a real kernel pid and process name (`service.rs:300`, `:323`) | about 232 (both launch paths) | A test constructor that takes the authorizer and a process matcher |
| Supervisor fixed `/space/run/firecracker/sandboxes`, `/proc/<pid>/cgroup`, `/sys/fs/cgroup/space` (`inventory.rs:114`, `:141`; core `cgroup_paths.rs:5`) | about 157, including `terminate_sandbox_cgroups` and the `0a210e5` fix lines | Run-directory and cgroup-root fields, as `ExecutionStore::at` already has (`execution_store.rs:30`) |
| Entrypoints that read the process environment and fixed resources: `run_reconcilers` (`reconcilers.rs:189`), net-gateway `run` (fixed `SANDBOX_HOST_CONTROL_SOCKET`, `run.rs:294`), volmgr `run` (fixed `SERVICE_MUTEX_PATH`, `run.rs:32`), internal-api `run` (binds and reads env internally, `run.rs:251`) | about 140 + 75 + 190 + 75 | `from_environment(&dyn Environment)` (the pattern in `orphaned_sandboxes.rs:192`), a pre-bound listener, and socket and mutex paths as config fields |
| net-gateway `IP_TRANSPARENT` listeners and UDP `SO_MARK` sockets (`run.rs:344`, `udp_proxy.rs:1471`–`1522`, `net-common orig_dst/linux.rs`) | about 230 (UDP) + 320 (run) | A socket factory, or a test in the privileged group |
| net-gateway fixed guest address `GUEST_IPV4_ADDR` (`ingress/mod.rs:711`) | about 110 (service-port ingress) | A guest-connector function on the ingress state |
| Concrete `Arc<KubernetesSubstrate>` in `quarantine_recovery.rs` and the suspended reaper's pod access (`suspended_execution_reaper/mod.rs:339`) | about 60 + 50 | `AbsenceProbes` and get/delete-pod traits |
| Compile-time intervals and budgets measured with `Instant` rather than Tokio time: guest `IDEMPOTENCY_RECORD_TTL`, `PULL_WATCHER_REAPER_INTERVAL`, host `GUEST_SIGNAL_RATE_WINDOW`, transient tick budget, fanout cache TTL | about 70 | An injectable clock, or constructor parameters. net-gateway's retry and deadline constants are reachable with `start_paused` today. |
| `mlock2` is process-wide; `/proc/self/maps` (`memlock.rs:47`, `:55`) | about 22 | `lock_mappings(maps, lock_fn)` |
| Fixed `eth0` scan (`port_forward/mod.rs:663`) | about 12 | Pass an interface or bind-IP resolver |
| *Measurement only:* worker `env_clear` (`filesystem/worker/linux.rs:751`), `pre_exec` bodies (`process.rs:776`, `:859`, `cgroup.rs:256`), and `async_trait` bodies | about 55 + 20 + 3,670 not measurable | Allow `LLVM_PROFILE_FILE` through the worker allowlist under `cfg(coverage)`; move `pre_exec` bodies into plain functions; keep handler logic in inherent functions |

These need no seam, only a test:
- `KubeLeaseStore`, with a `tower::service_fn` fake apiserver (the pattern in
  `node_drain/kube_delete/kube_delete_tests.rs`);
- the orphan sweep, with `SweepConfig { enabled: true, .. }`;
- the generic `run` loops, under `tokio::test(start_paused)`;
- `node_drain/aws.rs`, which is already generic over `AsgTerminateClient`;
- volmgr dispatch, with `new_for_test` and `MockObjectStore`.

## Q4 — Characterization-test tasks

Each task adds tests only, owns the files in the ranked table, and needs no
seam. Estimates are the reachable adjusted-uncovered lines of the target
functions, discounted.

1. **Node-drain Postgres sink** (+300). Drive the 18 never-called `SubstrateSink` methods through `NodeDrainSink` on a `with_suspend_execution` sink (24 existing uses) against `pg_test_db`. Assert the Rust refusals at `store.rs:2290`, `:2486`, `:2532`, `:2580`, `:2628`, `:2726`, `:3504`. Add an `evacuate_sandbox` case whose fake suspend succeeds, reaching the commit at `:2914`.
2. **Transient-sweeper Postgres sink** (+280). Run `SubstrateSweepSink` over `Substrate::Direct(DirectSubstrate::new(pool))`. Cover every compare-and-set and both undo statements, using the production SQL instead of the pasted copy. The row helpers are private to `transient_lifecycle_sweeper/tests.rs`, so copy them rather than edit that file.
3. **spaced-guest process retention and guards** (+40 lines, about a dozen guards; densest churn). Backdate a completed record (`backdate_completion`, `process.rs:1030`), then `get_record`, `list`, and `reserve` at the 128-record limit. Also: `ensure_reaper` on an empty registry; signal, input, close-stdin and resize on an unknown id and an exited id; disabled and closed stdin; a nonexistent executable (reaches the `ProcessLaunch::publish` failed-start cleanup and the unpublished claim drop).
4. **net-gateway TLS termination** (+180). Wire `local_provider_with_policy().requester()` into `TlsInterception::with_upstream_roots` for real handshakes. Cover `terminate_tls`, `accept`, `connect`, `admit_handshake` and handshake-pool saturation.
5. **internal-api snapshots and Postgres quarantine refusals** (+120). Fill `PublishedSandboxState.snapshots`. Republish `quarantined` for an already-quarantined sandbox and for one owned by another node. Assert survivors of the absent-stopped delete and `prune_absent_node_sandboxes`.
6. **volmgr dispatch** (+70). Call `JobExecutor::execute` with push, pull, unknown-kind and malformed `Job`s, plus a source-missing push (`:4640`) and missing-after-put (`:4855`).
7. **spaced-host relay surface** (+300). Use `Fixture` + `MockBridgeGuest` + `SpacedHostClient` for rename, `write_batch_as_user`, `write_stream_with_idempotency`, the watch relays, `get_modified_files`, `list_processes` and `notify_resume`.
8. **`publish_node_state` fields** (+110). Over RPC, send `node_status`, `template_inventory_complete = true` with templates, `execution_inventory`, a status change with `state_changed_at`, and a publication from a control-plane role.
9. **Data-plane route reset** (+180). Call `reset_sandbox_service_route`, `org_revocation_epoch` and the `sandbox_runtime_access` chain against `pg_test_db`.
10. **Warm-pool orphan sweep** (+180). `SweepConfig { enabled: true }` with the `warm_pool/tests.rs` `service_fn` and reflector fakes: cold cache, live slot, ownership reappeared, dry-run, DELETE 200/404/409/500, cap deferral.
11. **net-gateway hydration** (+110). Under `start_paused`: 12-retry exhaustion, the 30 s deadline, an unparsable reference, Draining/Stopped/timeout TCP-policy denials, `set_stopped` with a policy.
12. **volmgr push submission** (+120). `test_service` + `request_from_peer`: push submit (including the dedup-hit orphan unlink), get/list/cancel for not-found, not-a-pull and bad scope, attach/detach with a bad id or missing spec.
13. **Guest node-local guards** (+40, about 10 guards). Idempotency expiry (a second lookup within the prune interval, plus a test-only backdating hook), 16,385 ledger events on one path, port-forward open after shutdown and with port 0.
14. **Supervisor guards** (+85). Idempotent rebind, superseded bind and retire, absent binding with `allow_absent`, `store_status`, `remove`/`cpu_budget` on an absent directory, 32-slot exhaustion, and the read-only `process.rs`/`inventory.rs` helpers. Launch, stop and cgroup termination need the Q3 seams.
15. **Receipt and batch validation** (+55). `parse_published_transition_receipt` rejections (`:100`–`248`), the omission path (`:1779`–`1794`), whole-batch rejections (`:1848`–`1927`), `quarantine_recovery_projection`.
16. **`KubeLeaseStore`** (+90). Create, create 409, self-renew, peer denied, zero-duration takeover, replace 409, release when not held / 409 / 500.
17. **Guest client and networking** (+90). `SpacedGuestClient::new_with_config` against `GuestFixture`; `BridgeGuestNetworkingClient` open and close.
18. **Trigger fanout loop** (+60). `run`, `drain_ready_triggers`, `sweep_dead_trigger_rows_for_config`, a resolver error, and the cluster-role refusal (`:477`).
19. **Replica provenance** (+35). Valid, missing, bad JSON, wrong version, bad subject, bad URI.
20. **Claim generation** (+12, 3 guards). Re-enqueue between claim and complete or dead-letter.

## Q5 — Fix churn against coverage

There are 57 commits since the import on these directories. 31 count as
fixes: `026e103 02d05cf 0a210e5 0b1f740 146ff9c 27605f4 3ea44f2 45c545c
5b6fae4 627d6e8 654d405 73702aa 7965372 82d37a3 83edff7 86b6b96 8961cf2
90ce127 a1b4bef cf0a601 d11e912 d5412a2 d5bc2db d874d25 dcd3347 dd2f68c
eed7393 f045410 f98b21a f9050d7 fd42141`. Features, refactors, test-only and
infrastructure commits are excluded.

| Production file | Fix commits (all) | Line coverage | Fix-authored lines covered | Verdict |
|---|---|---|---|---|
| spaced-guest `process.rs` | 9 (10) | 1,027/1,167 (88%) | 468/538 (87%) | Main paths pinned. The failure and cleanup arms the fixes added are not: `publish` failed start, reaper idle exit, `drain_process_output_tasks`, closed-registry refusal. |
| spaced-guest `filesystem/worker/linux.rs` | 4 (5) | 462/577 (80%) | 101/108 (94%) | Good. The refused-before-start markers (`8961cf2`) are not exercised. |
| spaced-guest `lib.rs` | 4 (5) | 353/440 (80%) | 18/21 (86%) | Only the `8961cf2` refusals at `:474` and `:665` are missing. |
| spaced-guest `cgroup.rs` | 3 (4) | 58/317 (18%) | 1/12 (8%) | **Weakest hot file.** Lines from the `532113a` envelope split (not counted as a fix) are 56/162. Only the pure parsers are tested; needs the cgroup-path seam. |
| spaced-common `lib.rs` | 3 (4) | 57/61 (93%) | 8/8 | Covered. |
| spaced-host `bridge.rs` | 3 (3) | 836/997 (84%) | 47/49 | Covered, apart from the vsock arm of `connect_bridge_guest_channel`. |
| spaced-guest `filesystem.rs`, `router.rs`, `watch*.rs`, `write*.rs`, `idempotency.rs` | 1–3 each | 80–97% | 73–100% | Covered. |
| volmgr `run.rs` | 2 (7) | 134/370 (36%) | 4/6 | **Hot and weak.** Lines from the non-fix `#220`/`#230` commits (`:508`, `:536`–`540`, `:620`) never run either. Needs the `SERVICE_MUTEX_PATH` seam. |
| internal-api `adapter.rs` | 2 (4) | 184/274 (67%) | 19/23 | Mostly unmapped `async_trait` bodies; `template_read_scope` arm uncovered. |
| net-gateway `tcp_proxy/tls.rs` | 2 (2) | 118/562 (21%) | 17/60 (28%) | **Hot and weak:** `admit_handshake` (`#156`) has no test. Task 4. |
| internal-api `metadata.rs` | 2 (2) | 563/810 (70%) | 30/32 | Fix lines covered; the facet-condition writers are not. |
| volmgr `executor.rs` | 0 (5) | 2,587/3,509 (74%) | – | Leaf logic covered; the dispatch arms the changes touched are not. Task 6. |
| volmgr-svc `identity_handoff.rs` + `journal.rs` | 1 (1) | 97% / 96% | 693/715 | Covered, but only in the privileged half (0% without it). |
| supervisor `service.rs` + `cgroups.rs` | 1 (1) | 42% / 6% | 1/79 (1%) | **`0a210e5` is effectively untested.** Needs the cgroup-root and authorizer seams. |
| cluster-controller `reconcilers.rs` | 1 (2) | 29/426 (7%) | 0/4 | Wiring only; needs the entrypoint seam. |
| cluster-controller `public_operation_dispatcher.rs` | 1 (4) | 374/461 (81%) | 57/63 | Covered, except the claim-token fence (`:809`). |

**Do the hottest files have the weakest tests? No, on the whole:**
- Spearman ρ between fix-commit count and line coverage is 0.00 (221 files,
  each with 20 or more lines).
- By fix-commit count:

  | Fix commits per file | Files | Line coverage |
  |---|---|---|
  | 0 | 176 | 72.0% |
  | 1 | 35 | 72.2% |
  | 2–3 | 19 | 72.2% |
  | 4 or more | 3 | 84.3% |
- Fix-authored lines are 86.6% covered, against 72.5% for all lines.

**The exceptions** are where the fix touched a fixed system resource (cgroup
paths, the TLS handshake path, the service mutex). The fix landed with tests
of its pure helpers, but not of its integration:
- spaced-guest `cgroup.rs`
- net-gateway `tcp_proxy/tls.rs`
- volmgr `run.rs`
- the supervisor `0a210e5` lines

Tasks 3, 4 and 6 and the cgroup, authorizer and mutex-path seams in Q3 target
exactly these.

## Open questions

1. **`async_trait` mapping.** About 3,670 handler lines have no coverage
   regions under rustc 1.98.0. A CI coverage gate would read these files as
   better covered than they are. Is this worth a rustc issue, or should we
   rely on the convention of keeping handler logic in inherent functions?
2. **Unmeasured callers.** `pplx-space-api`, `sandbox-teardown`, node-ctl and
   data-plane call some of the 0% functions here:
   - the pool store verbs;
   - `enqueue_triggers_tx`;
   - `finalize_sandbox_db_delete_tx`;
   - `reset_sandbox_service_route`.

   A workspace-wide run would show whether those callers cover them.
3. **`node_drain/store.rs:2914`.** A lost compare-and-set on the
   suspending → suspended commit is not detected. Is that intended (on the
   grounds that the claim makes it unreachable)?
4. **Child profiles.** Should the identity worker pass `LLVM_PROFILE_FILE`
   through under `cfg(coverage)`, so the worker loop can be measured?

## Reproduction

Build the CI image with the coverage tools:

```sh
docker build --build-arg SPACE_CI_UID="$(id -u)" -f ci/Dockerfile.ci -t space-ci:local <dir with ci/Dockerfile.ci and rust-toolchain.toml>
# on top of it:
rustup component add llvm-tools-preview --toolchain 1.98.0-x86_64-unknown-linux-gnu
rustup toolchain install nightly --profile minimal --component llvm-tools-preview
cargo install cargo-llvm-cov --version 0.9.1 --locked
```

Start the test services:

```sh
bash ci/scripts/ci-test-services.sh start <network> <owner> > services.env
```

Inside the container, the unprivileged half runs as uid 1000 with
`--cap-drop=ALL --security-opt=no-new-privileges --network <network> --env-file services.env`.
The privileged half runs as root with `--privileged` and the Docker socket.

```sh
eval "$(RUSTC_BOOTSTRAP=1 cargo llvm-cov show-env --branch --sh)"; export RUSTC_BOOTSTRAP=1 SPACE_CI_REQUIRE_TEST_DEPS=1
cargo build --locked -p pplx-space-db-test-fixtures --bin space-nextest-postgres-setup
cargo nextest run --locked --all-targets --all-features -p <each of the 27 packages> \
  --run-ignored all --ignore-default-filter --retries 1 --no-fail-fast --test-threads 30 \
  -E 'not group(privileged)'        # then, as root: -E 'group(privileged)'
mv crates/libs/spaced/guest/server/default_*.profraw target/   # env-cleared children
LLVM_COV="$(rustc +nightly --print sysroot)/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-cov" \
  cargo llvm-cov report --branch --json --output-path coverage.json
```

To rerun, restart the test services first: the shared Postgres keeps
`space_template` between runs. Per-function rows come from mapping each mapped
line to its innermost `syn` `fn` item span.

## Summary (repeated)

- **Overall:** 27 packages, 42,162/58,134 lines (72.5%) and 4,000/6,182
  branch outcomes (64.7%).

  | Area | Line coverage |
  |---|---|
  | spaced guest | 80.8% |
  | spaced host | 71.3% |
  | spaced-common | 87.9% |
  | net-gateway | 71.9% |
  | cluster-controller | 68.8% |
  | internal-api | 82.5% |
  | vol-mgr | 65.3% |
  | runtime supervisor | 46.5% (unchanged since 2026-10-10) |
- **Production stores behind fakes are the weak spots.** cluster-controller's
  node-drain sink (36%), transient-sweeper sink (14%) and lease store (27%)
  never run their Postgres `sandboxes.lifecycle` writers or their undo SQL.
- **Refusal paths are mostly never exercised:**
  - Postgres quarantine compare-and-set aborts and projection claims in
    internal-api;
  - the node-drain `flipped` and `rows_affected` checks;
  - spaced-guest record expiry and eviction;
  - idempotency expiry;
  - supervisor superseded and idempotent execution binds;
  - volmgr-svc journal owner, mode and nlink refusals.
- **Seams:** cgroup and `/proc` paths (spaced-guest, supervisor), the
  credential-manager `OnceLock`, the node-manager socket, node drain's
  `kubernetes()`, the supervisor peer authorizer and the env-reading
  entrypoints block about 2,000 lines (all the Q3 seams together, about
  2,900).
- **Churn:** hot files are not weaker on the whole (ρ = 0.00; fix-authored
  lines 86.6% covered). The exceptions are guest `cgroup.rs`, net-gateway
  `tls.rs`, volmgr `run.rs` and the supervisor `0a210e5` lines.
- **Measurement:** `#[tonic::async_trait]` bodies (about 3,670 lines) are
  invisible to LLVM coverage.

| # | Task | Owns | Est. gain |
|---|---|---|---|
| 1 | node-drain Postgres sink | `node_drain/store/sink_postgres_tests.rs` | +300 |
| 2 | transient-sweeper Postgres sink | `transient_lifecycle_sweeper/store/substrate_sink_tests.rs` | +280 |
| 3 | **spaced-guest** process retention and guards | `process/retention_tests.rs` | +40, about a dozen guards |
| 4 | net-gateway TLS termination and admission | `tcp_proxy/tests/tls_terminate.rs` | +180 |
| 5 | internal-api snapshots and Postgres quarantine refusals | `tests/published_state.rs` + support | +120 |
| 6 | volmgr job dispatch | `executor/dispatch_tests.rs` | +70 |
| 7 | spaced-host relay surface | host `tests/it/relay_surface.rs` | +300 |
| 8 | internal-api `publish_node_state` fields | `src/tests/publication/node_state.rs` | +110 |
| 9 | internal-api data-plane route reset | `data_plane_sessions/tests.rs` | +180 |
| 10 | warm-pool orphan sweep | `warm_pool_orphan_sweep/sweep_tests.rs` | +180 |
| 11 | net-gateway hydration guards | `state/hydration_tests.rs` | +110 |
| 12 | volmgr push submission and job RPCs | `service/push_submit_tests.rs` | +120 |
| 13 | spaced-guest node-local guards | `idempotency/expiry_tests.rs`, `watch/ledger/overflow_tests.rs`, `port_forward/guard_tests.rs` | +40, about 10 guards |
| 14 | supervisor execution and inventory guards | `service/execution_guard_tests.rs`, `process/readonly_tests.rs`, `inventory/readonly_tests.rs` | +85 |
| 15 | internal-api receipt and batch validation | `src/published_state_tests.rs` | +55 |
| 16 | `KubeLeaseStore` | `leader_election/store/kube_lease_store_tests.rs` | +90 |
| 17 | spaced-guest client and networking | guest `tests/it/client_and_networking.rs` | +90 |
| 18 | trigger fanout loop | `trigger_fanout/loop_tests.rs` | +60 |
| 19 | volmgr-client provenance | `provenance_tests.rs` | +35 |
| 20 | trigger-queue claim generation | `claim_generation_tests.rs` | +12 and 3 guards |
