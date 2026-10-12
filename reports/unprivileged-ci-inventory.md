# What the Rust test suite needs root, privileges, host networking or Docker for

Base commit `7a41941f3a156c447e3271d0f20e2e66197c5908`. This is a report only: no code or
CI configuration was changed. Every claim below that is marked *measured*
comes from running the full CI nextest inventory (8871 tests, 142 binaries, the same
count as the Rust tests job on `main`) inside the image built from
`ci/Dockerfile.ci`, with one capability changed at a time.

## Decision summary

1. **Only 39 of 8871 tests need anything beyond an ordinary user (measured).** With no root,
   no `--privileged`, no `--network host` and no Docker socket, 8832 tests pass. The
   other 39 fail, and each failure was traced to a single capability:
   - 23 need root alone;
   - 4 need CAP_SYS_ADMIN, loop devices, or a seccomp relaxation;
   - 12 need the Docker socket.

   Two more tests pass only by skipping silently when not root, so they also belong in
   the privileged set.
2. **No test needs host networking (measured).** The only thing that does is
   `ci/scripts/setup-nextest-redis.sh`, which publishes `127.0.0.1:<port>`. Under Docker
   without host networking, that breaks 13 Redis-backed `pplx-space-api` tests.
3. **Mechanism:** add a nextest test group `privileged` to `.config/nextest.toml`,
   defined by one explicit filter. The default job then runs `-E 'not group(privileged)'`
   and the opted-in job runs `-E 'group(privileged)'`. The two sets are complements, so
   together they cover the inventory by construction. This was measured with an overlay
   config (the filter is in [Proposed split](#proposed-split)):
   - `not group(privileged)`: 8832 of 8832 pass unprivileged (two runs, s1b and s6).
     Two earlier runs, s1 and s3, used a regex typo that left out one router test,
     and that test failed.
   - `group(privileged)`: 39 of 39 pass, both with `--privileged` and with a smaller
     capability set.
4. **Two jobs, one build.** Keep a single runner and a single build.
   - The default step runs as a non-root user with default capabilities, a bridge
     network and no Docker socket. Postgres and Redis run as sibling containers that the
     runner starts.
   - A second `docker run` executes the 39 privileged tests from the same build with
     `cargo nextest run --binaries-metadata … --cargo-metadata …`. This was measured: no
     rebuild, and no root-owned files left in `target/`.
   - Expected wall-clock cost is about 20–40 s per CI run.
5. **Fix rather than move, where the privilege is incidental:**
   - 2 `pplx-space-db-migrate` tests use a Docker socket only because they are missing
     from the Postgres setup-script filter (verified: they pass against the shared
     server).
   - The build-mgr copy test escalates through `sudo`.
   - The net-gateway exemption test needs CAP_NET_RAW only for `SO_MARK`.
   - Two opt-in btrfs tests depend on the fixed node path `/space/cache`.
   - The net-gateway-client test writes the fixed path `/space/run/net-gateway`.
6. **Requirements the unprivileged image must meet (measured):**
   - The test user needs a passwd entry. Without one, 94 of 257 spaced tests fail.
   - Its supplementary groups must match `/etc/group`. An undeclared `--group-add` gid
     breaks 32 spaced process tests.
   - It needs a writable `CARGO_HOME`.

## Ranked actions

| # | Action | Kind | Evidence |
|---|---|---|---|
| 1 | Add `[test-groups.privileged]` and one override listing the 39 tests. Run the default job as non-root, without `--privileged`, `--network host` or `docker.sock`, using `-E 'not group(privileged)'`. | split | measured: s1b, s6 runs, 8832/8832 |
| 2 | Let `setup-nextest-postgres.sh` and `setup-nextest-redis.sh` use a runner-provided server instead of `docker run`. Make the Redis script probe the container IP as the Postgres one already does. | CI script | measured: shim runs; r4 (Redis refused) |
| 3 | Add `or (package(pplx-space-db-migrate) and test(postgres_baseline_tests::))` to the Postgres setup filter. Two tests then stop needing Docker. | config fix | measured: s1 overlay run |
| 4 | Add a non-root `USER` with a passwd entry and a writable `CARGO_HOME` to the CI image (or the job). Never reach the socket through an undeclared `--group-add`. | image | measured: u1 (94 fail), e2 (32 fail), e3 (pass) |
| 5 | Privileged step: root, `--privileged`, the Docker socket for the 10 bootstrap-role tests, no host network. Reuse the build through nextest's reuse-build flags. | split | measured: s2, s4, m2 (39/39) |
| 6 | Host-safety fixes: inject the cache root used by `btrfs_send_transport` and `artifact_fs::receive`; drop `sudo` escalation from the build-mgr copy test; stop the net-gateway-client test creating `/space/run/net-gateway`. | bug | measured: b1 (2 fail); code refs below |
| 7 | Make root-only tests fail closed in CI instead of `return`ing (`gateway_isolation`, `net-gateway-client`). A privileged test that drifts out of the group then fails loudly. | robustness | measured: r0v skip output |
| 8 | Make the net-gateway exemption test independent of `SO_MARK` (CAP_NET_RAW). It can then leave the privileged set. | bug | measured: e4 |
| 9 | Run the btrfs opt-in tests in the privileged step. This needs `btrfs-progs` in the image and a loop-mounted btrfs. Today 6 pass, 2 fail on `/space/cache`, and 1 skips as root. | coverage | measured: b1 |
| 10 | Move the M1 SDK side check (needs Docker) out of the unprivileged container, into a runner-side step or the privileged step. | CI script | measured: d1 |

## Method and environment

Every run used the CI image built locally from `ci/Dockerfile.ci`. Pulling the
published GHCR tag was denied with the available token, so the image was built
instead. The same image was used for all runs, plus a one-line derived image that adds
`useradd -u 1000 tester` so a non-root user has a passwd entry.

**Build.** The workspace was built once as uid 1000:
`cargo test --workspace --locked --all-targets --all-features --no-run`, with the
environment `run-rust-checks.sh tests` exports:
- the Wild linker through gcc;
- `CARGO_PROFILE_DEV_DEBUG=line-tables-only`;
- the two `--config profile.test.package.*.opt-level=0` overrides;
- `TESTCONTAINERS_RYUK_DISABLED=true`;
- `SPACE_CI_REQUIRE_TEST_DEPS=1`, `CI=true`, `GITHUB_ACTIONS=true`.

**Test runs.** Each run used CI's exact nextest invocation:
`cargo nextest run --workspace --locked --all-targets --all-features --run-ignored all
--ignore-default-filter --retries 1 --no-fail-fast --test-threads 30`. A full run took
22–57 s, so every configuration ran the whole workspace. No run was split per package.

**Postgres and Redis.**
- *Runs without the socket:* the runner started a Postgres 16 container (tmpfs data,
  `fsync=off`, `max_connections=200`, as the setup script starts it) and a Redis 5
  container on a user-defined bridge network. The two setup scripts were bind-mounted
  over with shims, and the repository files were not edited. The Postgres shim runs the
  real `space-nextest-postgres-setup` binary against the sidecar and writes
  `SPACE_TEST_POSTGRES_ADMIN_URL`; the Redis shim writes `SPACE_TEST_REDIS_URL`. The
  appendix has the shims.
- *Runs with the socket:* the real setup scripts ran, as in CI.

**Test host.** A 32-vCPU x86_64 Linux 6.12 development host running Docker 29. The
daemon uses the builtin seccomp profile, cgroup v2, and no AppArmor. The GitHub
`ubuntu-24.04-16core` runner differs in ways listed under [Limitations](#limitations).

## Results by capability

Counts are final failures after nextest's single retry. "Flaky" tests passed on retry
and are not counted.

| Run | uid | added back | Postgres/Redis | failed | what changed vs baseline |
|---|---|---|---|---|---|
| r0 ×3, r0v | 1000 | nothing (bridge network) | sidecars | **39** | baseline; identical failing set in all 4 runs |
| r1 | 0 | root alone | sidecars | 16 | 23 root-only tests pass |
| r2 | 0 | root + `--privileged` | sidecars | 12 | only the 12 Docker-socket tests remain |
| r2b | 1000 | `--privileged` without root | sidecars | 36 | the router pair and `real_peers` pass (seccomp lifted) |
| r3 | 1000 | `--network host` | sidecars | 39 | **no change**: no test needs host networking |
| r4 | 1000 | socket (`--group-add <sock gid>`), real setup scripts | real scripts | 72 | +12 db-migrate pass; 13 Redis tests refused; 32 spaced fail on groups |
| r4b | 1000 | socket (`--group-add`), sidecars | sidecars | 59 | +12 db-migrate pass; 32 spaced group failures |
| e3 | 1000 | socket, gid declared in `/etc/group` | sidecars | 27 | exactly the 12 db-migrate tests pass |
| e7 | 1000 | socket + host network, real scripts | real scripts | 59 | Redis works again; 32 group failures remain |
| r5 ×2 | 0 | full CI flags (`--privileged --network host` + socket) | real scripts | **0** | matches CI |
| e1 | 1000 | `--security-opt seccomp=unconfined` | sidecars | 36 | router pair and `real_peers` (user namespace) pass |
| e2 | 1000 | `--group-add <undeclared gid>` only | sidecars | 71 | +32 spaced failures: groups, not the socket |
| e4 | 0 | root, `--cap-drop NET_RAW` | sidecars | 17 | net-gateway exemption test fails again |
| e5a | 0 | root + `--cap-add SYS_ADMIN` | sidecars | 15 | `rejects_bind_mounts_in_private_state` passes |
| e5b | 0 | e5a + seccomp/apparmor unconfined | sidecars | 13 | router pair passes |
| e5c | 0 | e5b + `/dev/loop-control` + `b 7:* rmw` | sidecars | 12 | loop-device test passes |
| e6 | 0 | root + `--cap-add SYS_PTRACE` | sidecars | 14 | router pair passes (`pidfd_getfd`) |
| n1 | 1000 | nothing, `--internal` network (no egress) | sidecars | 40 | +1: the S3 public-bucket test |
| u1 | 1000, **no passwd entry** | nothing | sidecars | 94 of 257 (spaced packages only) | "current user exists" panics |

Runs that check the proposed split, all with the overlay `nextest.toml`:

| Run | uid | flags | filter | result |
|---|---|---|---|---|
| s1, s3 | 1000 | none | `not group(privileged)`, earlier router regex | 8832 / 8833; the one router test the regex missed fails, as in r0 |
| s1b, s6 | 1000 | none | `not group(privileged)` | 8832 / 8832 pass |
| s2 | 0 | `--privileged` + socket, real scripts | `group(privileged)` | 39 / 39 pass |
| s4 | 0 | `--privileged` + socket, sidecars, no host network | `group(privileged)` | 39 / 39 pass |
| s5 | 0 | `--cap-add SYS_ADMIN --cap-add SYS_PTRACE --device /dev/loop-control --device-cgroup-rule 'b 7:* rmw'` + socket | `group(privileged)` | 39 / 39 pass |
| m2 | 0 | `--privileged` + socket, reuse-build (no cargo build) | `group(privileged)` | 39 / 39 pass; no root-owned files in `target/` |
| d1 | 1000 | none | side checks | doctests and feature-isolation pass; M1 fails: no Docker daemon |
| b1 | 0 | `--privileged`, loop-mounted btrfs, both btrfs opt-in env vars set | btrfs tests | 62 / 64 pass; 2 fail on `/space/cache` |

## Classification of the failing tests

### Needs root uid only (23)

Default Docker capabilities as uid 0 are enough. All of these pass in r1.

| Package | Test | Why |
|---|---|---|
| pplx-space-volmgr-svc (bin) | `identity_handoff::tests::` × 15: `a_live_predecessor_blocks_all_ownership_changes`, `expected_identity_prevents_implicit_rollback`, `hot_sqlite_journal_survives_handoff_and_recovers_committed_rows`, `initial_handoff_rejects_replaceable_staging_parent`, `initial_handoff_rejects_service_writable_private_state`, `installation_trust_checks_ancestors_and_symlink_boundaries`, `killed_handoffs_resume_or_reverse_without_losing_data`, `pending_record_never_recreates_missing_directories`, `published_record_never_replays_file_ownership`, `replacement_between_validation_and_sealing_is_rejected`, `replacing_published_staging_cannot_grant_legacy_files`, `retained_state_round_trips_without_replacing_mutex_or_bytes`, `separated_service_cannot_launder_a_legacy_owned_file_on_restart`, `services_cannot_read_or_forge_recovery_records`, `unsafe_retained_entries_fail_without_reowning_external_data` | `assert` root at `identity_handoff/tests.rs:71`; chown to service uids, setuid children |
| pplx-space-node-mgr | `clone_tree::tests::private_clone_blocks_foreign_uid_grafts_and_shared_clone_is_held` | asserts "run in a disposable root container" (`clone_tree/tests.rs:290`) |
| pplx-space-sbx-mgr | `transfer_directory::tests::distinct_service_uids_can_exchange_downloads` | switches between two service uids |
| pplx-space-credmgr | `run::volume_socket::tests::preflight_checks_real_permissions_without_replacing_live_socket` | chroot plus setuid 950 |
| pplx-space-credmgr | `run::tests::volume_socket::real_peers_obey_volume_listener_policy` | Root, *or* non-root with user namespaces. The non-root `unshare --user` fallback fails under Docker's default seccomp, and passes in e1 and r2b. Ubuntu 24.04 also restricts unprivileged user namespaces through AppArmor, so treat it as root. |
| pplx-space-spaced-guest-server | `worker_subprocess` (whole binary) | `AssumeIdentity`: EPERM |
| pplx-space-spaced-guest-server | `worker_telemetry` (custom harness) | asserts root (`worker_telemetry/linux.rs:61`) |
| pplx-space-build-mgr | `oci::assembly::tests::extraction_failure_is_not_masked_by_archive_sigpipe` | **Incidental.** `copy_tree` runs `command_as_root("tar")` (`assembly.rs:248`), which becomes `sudo tar` when not root. The image has no sudo, so it fails with ENOENT. See host-safety. |
| pplx-space-net-gateway | `tcp_proxy::tests::tls_interception::interception_exemptions_relay_original_tls_only_after_admission` | **Incidental.** The relay sets `SO_MARK` (`tcp_proxy/relay.rs:37`), which needs CAP_NET_RAW or CAP_NET_ADMIN. Root has NET_RAW by default; e4 drops it and the test fails. As non-root the relay fails and the test times out (`exempt host: Elapsed`). |

### Needs CAP_SYS_ADMIN, devices, or a seccomp relaxation (4)

| Package | Test | Minimum measured |
|---|---|---|
| pplx-space-volmgr-svc | `identity_handoff::tests::rejects_bind_mounts_in_private_state` | root + CAP_SYS_ADMIN (`unshare --mount` and a bind mount). Passes in e5a. |
| pplx-space-node-mgr | `loop_devices::tests::ensure_capacity_creates_attachable_devices_past_max_loop` | root + CAP_SYS_ADMIN + loop block devices (`/dev/loop-control`, cgroup rule `b 7:*`). Passes in e5c. **Mutates the host kernel**; see host-safety. |
| pplx-space-spaced-guest-server | `filesystem::router::tests::a_read_streams_from_the_workers_descriptor_without_relaying_through_it` | `pidfd_getfd`, which Docker's default seccomp profile allows only with CAP_SYS_PTRACE. Passes as root + SYS_PTRACE (e6), and as non-root with seccomp unconfined (e1). **Root is not required.** Without it the router silently falls back to relaying the bytes, so the test fails on its byte count. |
| pplx-space-spaced-guest-server | `filesystem::router::tests::a_read_through_a_workers_descriptor_ends_with_the_worker` | same; fails with `the dead worker is retired: Elapsed` |

### Needs host networking (0)

No test. In r3, adding `--network host` to the baseline changed nothing. Host networking
matters only to `setup-nextest-redis.sh`, which publishes and advertises
`127.0.0.1:<port>`. With the socket but a bridge network (r4), 13 tests get
`Connection refused`:
- `client_concurrency::tests::*` × 9;
- `cluster_admission::tests::*` × 4.

`setup-nextest-postgres.sh` already connects to the container IP when it is routable.
No test was seen relying on any other service local to the host.

### Needs the Docker socket (12, of which 2 are fixable by config)

| Package | Tests | Why |
|---|---|---|
| pplx-space-db-migrate | `bootstrap_roles::tests::postgres_bootstrap_tests::` × 10: `app_lock_timeout_converges_across_its_provisioning_lifecycle`, `assuming_the_migrator_role_installs_its_role_level_timeouts`, `bootstrap_adopts_routines_behind_admin_run_migrations`, `bootstrap_strips_preexisting_privileges`, `credential_references_enforce_triggers_and_grants`, `first_bootstrap_adopts_admin_owned_tables_routines_and_types`, `fresh_database_bootstraps_migrates_as_migrator_and_fences`, `missing_passwords_provision_nologin_roles`, `ro_reads_api_keys_without_key_hash`, `ro_reads_publication_grant_metadata_without_authorization_id` | Each needs a virgin server for cluster-global roles, so `pg_test_support.rs:37` starts its own testcontainer. |
| pplx-space-db-migrate | `tests::postgres_baseline_tests::all_postgres_migrations_apply_to_fresh_database`, `…::postgres_rejects_legacy_history_without_changing_data` | **Config gap.** They use `try_raw_pg_test_db` (database-local), but the Postgres setup filter in `.config/nextest.toml` excludes `pplx-space-db-migrate` except `user_password::`, so they start their own container. With the one-line filter addition they passed unprivileged against the shared server (s1). |

Outside nextest, three things in the job also need Docker:
- the two setup scripts (`docker run`);
- the M1 SDK side check (`scripts/check-m1-sdk-generation.sh`).

### Fails for an environment reason unrelated to privilege

| Cause | Tests | Evidence |
|---|---|---|
| Supplementary groups that are not in the user's `/etc/group` entry, e.g. `--group-add <docker gid>` used to reach the socket | 31 spaced-guest-server `process::` tests (13 unit, 18 `::it`) and `pplx-space-spaced-host-cli::output read_run_and_connect_preserve_bytes_without_waiting_for_process_exit` | `FailedPrecondition: current supplementary groups do not match requested guest user`. Fails in e2 (no socket); passes in e3 (gid declared). |
| uid with no passwd entry (`docker run --user N` on the stock image) | 94 of 257 tests in the spaced packages | `current user exists` panics (u1) |
| No outbound internet | `pplx-space-node-runtime-manager kernels::s3::tests::the_public_bucket_lists_the_kernels_for_a_line` | n1. The test is `#[ignore]`d, but CI runs ignored tests. |
| Timing flakes (passed on retry) | `pplx-space-common-grpc::retry deadline_clamps_retries`, `…caller_classified_cancelled_respects_deadline` (once, in r0); `pplx-space-internal-api-server tests::concurrent_begin_artifact_backup_is_idempotent` (r0v, r4) | not privilege-related |
| No `sudo` and not root | build-mgr copy test above | `NotFound` spawning `sudo` |

### Tests that should not touch the host (host-safety bugs)

1. **`loop_devices::tests::ensure_capacity_creates_attachable_devices_past_max_loop`**
   (`node/mgr/server/src/loop_devices/tests.rs:144`).
   - It runs in CI without any opt-in, because CI passes `--run-ignored all`.
   - It removes and recreates a `/dev/loopN` node, adds loop devices to the *running
     kernel*, and attaches one.
   - By its own comment, it leaves the devices behind.
   - In CI's `--privileged` container that kernel is the runner's.
   - Keep it only in the privileged step, on an ephemeral runner.
2. **`service::btrfs_send_transport::tests::durable_lease_survives_producer_removal_and_cancelled_grpc_reader`**
   (`btrfs_send_transport/tests.rs:34-36`).
   - It creates `/space/cache/firecracker/sandboxes/<uuid>` through the production
     constant `SANDBOX_CACHE_DIR`.
   - On a SPACE node or a provisioned dev host, that writes into the live sandbox cache.
     Elsewhere it fails with ENOENT (b1).
   - Inject the cache root.
3. **`service::artifact_fs::receive::tests::receive_keeps_destination_identity_through_scrub_and_publication`**.
   - Production `configure_receiver` hard-codes `btrfs receive -m /space/cache`
     (`artifact_fs/receive.rs:98`), so the test works only on a host that has the
     node's `/space/cache` btrfs mount (b1: `cannot open /space/cache`).
   - Derive the `-m` root from the pinned directory, or make it injectable.
4. **`net-gateway-client tests::sandbox_connections_ignore_replaced_alias_and_reject_legacy_only_gateway`**
   (`net/gateway/client/src/tests.rs:15-18`).
   - As root it creates the production `/space/run/net-gateway` directory and a temp dir
     inside it. On a real node that is the live gateway run directory.
   - As non-root it prints a message and passes.
5. **`oci::assembly::tests::extraction_failure_is_not_masked_by_archive_sigpipe`**.
   - When not root it runs `sudo tar`. On a developer machine with cached sudo
     credentials, a unit test therefore runs a root `tar` on the host.
   - The test is about pipe-status propagation and needs no privilege. Make the
     escalation injectable, or run the copy as the current user in tests.
6. **Job-level blast radius.** With `docker.sock` mounted into a root `--privileged`
   container, any test can start host containers or reach host devices, and none of it
   is visible.
   - Today the intended users are the setup scripts, the db-migrate testcontainers and
     the M1 generator.
   - The proposed default job removes this from 8832 tests.

The other root tests confine their effects to temp dirs or private namespaces.
`identity_handoff` chowns inside temp dirs; `gateway_isolation` uses setuid children in
a temp dir. They are root-only, not host-unsafe.

### Tests that pass for the wrong reason

**In an unprivileged job, these would be silent green.** They must be in the privileged
group (they are in the proposed filter) and should fail closed in CI:
- `pplx-space-node-mgr service::gateway_isolation::tests::isolated_gateway_cannot_access_vm_resources`
  prints `requires root; run in the Linux isolation test container` and returns.
- The net-gateway-client test above does the same.
- `service::encrypted_volumes::tests::ciphertext_publication_is_readable_without_exposing_private_backings`
  runs a reduced variant when not root. The uid-65534 read check runs only as root.
- `permission_tree` tests use the current uid instead of 100042 when not root.

The last two still pass and still test something unprivileged. To keep their root
branch, add them to the privileged step's filter as well (see below).

**In today's root CI, these never run their real path:**
- `spaced-guest-server filesystem::write::tests::a_file_the_caller_cannot_write_is_not_replaced`
  and `an_unreadable_directory_is_still_overwritten_by_replacement` return early as
  root. The unprivileged job would exercise them for the first time.
- `template-mgr delete_template_build_preserves_db_row_when_subvolume_removal_fails`
  skips as root.

**Opt-ins unset in CI, so these never run anywhere in CI.** They pass in every
configuration, including CI:

| Opt-in | Tests | Behavior when unset |
|---|---|---|
| `SPACE_NODEMGR_TEST_BTRFS_DIR` | 6 node-mgr: `device_backing…pinned_devices_survive_replacement_and_encrypted_reopen`, `artifact_fs…btrfs_pinned_operations…`, `artifact_fs::receive…`, `btrfs_send_source…retained_source_regenerates…`, `btrfs_send_transport…durable_lease…`, `prepared_rootfs…prepared_instance_and_clone_use_real_btrfs_subvolumes` | prints `not opted in`, passes |
| `SPACE_TEST_BTRFS_DIR` | 3 template-mgr `delete_template*` btrfs tests | prints `skipping`, passes |
| `SPACE_TEST_REAL_CONTAINERD` | build-mgr `real_containerd_exposes_a_flattened_read_only_rootfs` × 3 cases | silent return |
| `SPACE_TEST_REAL_OCI_ASSEMBLY` (+ `SPACE_TEST_SEED_ROOTFS`) | build-mgr `real_oci_image_assembles_with_the_space_guest_bundle` | silent return |
| `SPACE_RUN_CONTAINMENT_BENCHMARK`, `SPACE_RUN_NODE_OCCUPANCY_BENCHMARK`, `SPACE_RUN_FLEET_OCCUPANCY_BENCHMARK`, `SPACE_RUN_SPARSE_ACCOUNTING_BENCHMARK` | benchmarks (node-mgr netns, api, volmgr) | silent return |
| `SPACE_CI_MAIN_ONLY_TESTS` | snapshot-retention Postgres benchmark | prints `skipping` on PRs; set on main |

In b1, a privileged container with a loop-mounted 4 GiB btrfs and both directories set
gave these results:
- 4 of the 6 node-mgr tests passed, including `device_backing` with loop + dm-crypt.
- The two `/space/cache` tests failed.
- 2 of the 3 template-mgr tests ran and passed; the third skips as root.
- The image needs `btrfs-progs`, which `ci/Dockerfile.ci` does not install.
  `cryptsetup-bin` was added too, but it was not determined whether it is needed.

**"Passes because a host service is absent."** No such test was found among the
measured failures. Strict mode (`SPACE_CI_REQUIRE_TEST_DEPS=1`) turns a missing
Postgres/Redis/Docker into a hard failure, which is why the socket-less runs fail
loudly instead of skipping. The `SPACE-TEST-DEP-SKIP` lines in the logs come from tests
of the skip mechanism itself. The containerd and OCI opt-ins return before probing any
service. One environment-dependent skip was *not* verified:
`spaced-guest-server filesystem::write::upload` tests return early when `O_DIRECT` is
unsupported on the temp filesystem (`write/upload/tests.rs:207`).

## Existing opt-in mechanisms

- **`#[ignore]`.**
  - 30 attributes: 16 volmgr-svc, 9 node-mgr, 1 each in sbx-mgr, credmgr, net-gateway,
    node-runtime-manager and api.
  - No `cfg_attr(…, ignore)`.
  - CI runs them all (`--run-ignored all --ignore-default-filter`), so `#[ignore]`
    gates only local `cargo test`. It mixes several reasons: root, btrfs, network,
    benchmarks, and a dev-box-only TLS test. It cannot express the split.
- **Environment opt-ins.** These are the variables in the table above.
  `SPACE_NODEMGR_TEST_LOOP_DEVICES` does not exist; the loop-device test has no opt-in.
  Many other `SPACE_TEST_*` and `*_TEST_CHILD` variables are markers a test sets for
  its own re-executed child, not opt-ins.
- **Runtime uid checks.**
  - Some tests `assert!` root and fail closed: identity_handoff, clone_tree,
    transfer_directory, credmgr preflight, worker_telemetry.
  - Two `return` and pass: gateway_isolation, net-gateway-client.
  - Three `return` *when* root: the two spaced write tests and the template-mgr abort
    test.
  - Two change behavior by uid: encrypted_volumes, permission_tree.
- **`.config/nextest.toml`.**
  - `experimental = ["setup-scripts"]`.
  - Two setup scripts (`postgres` fail-closed, `redis` fail-open), each scoped by a
    filter in `profile.default`.
  - One `priority = 10` override.
  - No test groups, no other profiles, no `default-filter`.
- **Cargo features.** `test-hooks`, `test-capture` and `test-support` enable test
  plumbing, and CI's `--all-features` turns them on. None of them gates privilege.

## Proposed split

### Mechanism: a `privileged` test group plus fail-closed assertions

The recommended mechanism is a test group defined by one explicit filter in
`.config/nextest.toml`. The other mechanisms each fall short:

- **Env opt-in.** An unset opt-in reads as green, which is exactly the wrong-reason
  pass this split is meant to remove.
- **`#[ignore]`.** CI already runs ignored tests, and the attribute is overloaded.
- **Naming convention.** It would rename about 40 tests across 8 crates, and a new test
  that forgets the suffix still lands in the default job.

The group puts all the privileged choices in one reviewed place.
`-E 'group(privileged)'` and `-E 'not group(privileged)'` are exact complements, so the
two jobs cannot drop a test between them. Drift is caught as long as privileged tests
fail closed: a new root-only test that lands outside the group panics in the
unprivileged job. That is why action 7 matters. The group's `max-threads` also bounds
how much privileged work runs at once.

This filter was measured on this commit (s1b/s2/s4/s5/m2). It includes the router
regex fix made after s1 and s3.

```toml
[test-groups.privileged]
max-threads = 8

[[profile.default.overrides]]
filter = '''
(binary_id(pplx-space-volmgr-svc::bin/space-volmgr-svc) and test(/^identity_handoff::tests::/))
or (package(pplx-space-node-mgr) and test(=clone_tree::tests::private_clone_blocks_foreign_uid_grafts_and_shared_clone_is_held))
or (package(pplx-space-node-mgr) and test(=loop_devices::tests::ensure_capacity_creates_attachable_devices_past_max_loop))
or (package(pplx-space-node-mgr) and test(=service::gateway_isolation::tests::isolated_gateway_cannot_access_vm_resources))
or (package(pplx-space-sbx-mgr) and test(=transfer_directory::tests::distinct_service_uids_can_exchange_downloads))
or (package(pplx-space-credmgr) and test(/volume_socket::/) and test(/real_peers_obey_volume_listener_policy|preflight_checks_real_permissions_without_replacing_live_socket/))
or binary_id(pplx-space-spaced-guest-server::worker_subprocess)
or binary_id(pplx-space-spaced-guest-server::worker_telemetry)
or (package(pplx-space-spaced-guest-server) and test(/^filesystem::router::tests::a_read_(streams_from_the|through_a)_workers_descriptor/))
or (package(pplx-space-build-mgr) and test(=oci::assembly::tests::extraction_failure_is_not_masked_by_archive_sigpipe))
or (package(pplx-space-net-gateway) and test(=tcp_proxy::tests::tls_interception::interception_exemptions_relay_original_tls_only_after_admission))
or (package(pplx-space-net-gateway-client) and test(=tests::sandbox_connections_ignore_replaced_alias_and_reject_legacy_only_gateway))
or (package(pplx-space-db-migrate) and test(/^bootstrap_roles::tests::postgres_bootstrap_tests::/))
'''
test-group = "privileged"
```

Together with the Postgres setup-filter addition
`or (package(pplx-space-db-migrate) and test(postgres_baseline_tests::))`, this group
holds 39 tests:
- 16 identity_handoff;
- 10 bootstrap_roles;
- 2 credmgr;
- 2 spaced router;
- 2 spaced binaries;
- 1 each: clone_tree, loop_devices, gateway_isolation, transfer_directory, build-mgr,
  net-gateway, net-gateway-client.

After actions 6 and 8, the build-mgr and net-gateway entries can leave the group.

For tests whose root branch adds coverage (encrypted_volumes, permission_tree), the
privileged step can run `-E 'group(privileged) or <those tests>'`. They then run in
both steps. The complement property only needs to hold for the default job's filter.

### The two CI jobs

Both run on the same `ubuntu-24.04-16core` runner and share one build. They are two
steps within the existing "Rust tests" matrix entry.

**1. Rust tests (default, unprivileged)**
- `docker run --user <runner uid>:<runner gid>`, with a passwd entry for that uid and a
  writable `CARGO_HOME` or registry mount. HOME must be set.
- No `--privileged`, no `--network host`, no `/var/run/docker.sock`, no `--group-add`.
- `--network space-ci-tests`: a user-defined bridge the runner creates.
- Before the build, the runner starts Postgres (the setup script's flags) and Redis on
  that network, then passes their addresses in. The setup scripts' new no-Docker path
  initializes the template and publishes `SPACE_TEST_POSTGRES_ADMIN_URL` and
  `SPACE_TEST_REDIS_URL`, as the shims did.
- `SPACE_CI_REQUIRE_TEST_DEPS=1` stays.
- Runs the build, then
  `cargo nextest run … --run-ignored all --ignore-default-filter -E 'not group(privileged)'`,
  plus the doctests and feature-isolation checks (both measured passing unprivileged).
- Writes `cargo nextest list --list-type binaries-only --message-format json` and
  `cargo metadata` into `target/`. This took 2 s.
- Egress stays available for the S3 public-bucket test and the crate fetch.

**2. Rust privileged tests (opted in)**
- A second `docker run` of the same image as root, with `--privileged` and the Docker
  socket, after step 1. No host network.
- Runs `cargo nextest run --binaries-metadata target/…/binaries.json --cargo-metadata
  target/…/cargo-metadata.json --run-ignored all --ignore-default-filter
  -E 'group(privileged)'`.
- No cargo build runs, and in m2 no root-owned file was left in `target/`, so the
  target-cache pack step is unaffected.
- Measured smaller alternative to `--privileged` (s5): `--cap-add SYS_ADMIN --cap-add
  SYS_PTRACE --device /dev/loop-control --device-cgroup-rule 'b 7:* rmw'`. On Ubuntu
  runners, Docker's AppArmor profile may additionally block `mount` unless
  `--security-opt apparmor=unconfined` is added. That was not verified here, because
  this host has no AppArmor. Start with `--privileged` and tighten it later.
- The M1 SDK check (Docker) either moves here or into a runner-side step that reads the
  `m1-openapi.json` that step 1 generates.
- Later (action 9): install `btrfs-progs`, loop-mount a btrfs image, and set
  `SPACE_NODEMGR_TEST_BTRFS_DIR` / `SPACE_TEST_BTRFS_DIR` here.

The separate "Gateway traffic" matrix entry also uses `--privileged --network host`. It
is not part of the Rust test suite and was not examined.

### Wall-clock effect

For reference, the current `main` Rust tests job takes about 4 minutes:
- "Run Rust tests" takes 2m49s–3m15s;
- the warm build takes 1m50s;
- nextest takes 53.6 s for 8871 tests;
- the side checks overlap with nextest (doctests 39 s, feature isolation 39 s, M1 7 s).

Measured here (32 vCPU, 30 threads):
- the full suite as root: 53 s;
- the full suite unprivileged: 28 s;
- the default partition: 21.7 s;
- the privileged partition: 38–41 s, dominated by `killed_handoffs_resume_or_reverse_without_losing_data`
  (34 s here, 8.6 s in CI). The 10 bootstrap tests take 4–5 s each.

Estimated effect on CI:
- **Default step:** unchanged, about −0 to −5 s. The removed tests are not on CI's
  critical path.
- **Privileged step:** about +15–25 s if it runs after step 1. That is
  `killed_handoffs` at 8.6 s, the bootstrap tests in parallel, container start, and a
  testcontainers Postgres pull from the local cache. It could instead run concurrently
  with step 1's nextest run, adding about +0–10 s but competing for CPU.
- **M1 check:** about +7 s if serialized outside the container, 0 s if it overlaps.
- **Sibling Postgres/Redis:** about 0 s, since they start during the build.
- **Net:** about +20–40 s per run.

A separate job on its own runner would cost more: either a second warm build (≥1m50s)
or moving the nextest archive (2.66 GB; 26 s to create and 25 s to extract here, plus
artifact transfer). That alternative is not recommended.

## Limitations

- The runs used a 32-vCPU Linux 6.12 host with a Docker 29 daemon (builtin seccomp, no
  AppArmor), not the GitHub runner. Two kinds of result may differ there:
  - AppArmor-sensitive behavior: the min-caps set, and unprivileged user namespaces for
    `real_peers`.
  - Timing (`killed_handoffs` took 34 s here and 8.6 s in CI).
- The suite's test count and the passing CI-flag run (r5, 0 failures) match CI.
- "Host network" here is this host's network namespace, not a GitHub VM's. Since no
  test changed with it, this is unlikely to matter.
- The setup-script shims and the overlay `nextest.toml` were bind-mounted read-only
  over the repository files inside the container. The fixes they prove (actions 2 and
  3) are not implemented.
- Doctests were run unprivileged once (d1), with no failure-attribution matrix. They
  passed, so none was needed.
- The loop-device test passed in r5/e5c without adding devices, because this host
  already exposes 1200 loop nodes. Host loop/btrfs state was checked before and after:
  0 attached loop devices and no btrfs mounts left behind.
- The setup scripts' detached cleanup watcher dies with `docker run --rm`. In CI,
  `run-rust-checks.sh` removes the containers by label. This harness did not, so 8
  containers labeled by the harness were removed by hand afterwards. Any runner-started
  sidecars need the same label-based cleanup.

## Appendix: reproduction

All of this was harness code outside the repository.

```sh
# image
SPACE_CI_IMAGE_MIRROR= bash ci/scripts/ci-image.sh ensure space-ci:local
printf 'FROM space-ci:local\nRUN useradd --create-home --uid 1000 --user-group tester\n' |
  docker build -t space-ci:local-user -
# sidecars (one pair per run)
docker network create space-ci-tests
docker run -d --name pg --network space-ci-tests -e POSTGRES_HOST_AUTH_METHOD=trust \
  -e PGDATA=/var/lib/postgresql/data --tmpfs /var/lib/postgresql/data \
  public.ecr.aws/docker/library/postgres:16-alpine \
  -c fsync=off -c synchronous_commit=off -c full_page_writes=off -c max_connections=200
docker run -d --name redis --network space-ci-tests public.ecr.aws/docker/library/redis:5.0
```

Postgres shim, mounted at `ci/scripts/setup-nextest-postgres.sh`. The Redis shim
writes `SPACE_TEST_REDIS_URL=redis://$INV_REDIS_ADDR` in the same way.

```sh
#!/usr/bin/env bash
set -euo pipefail
[ -n "${NEXTEST_ENV:-}" ] || { echo "must run as a nextest setup script" >&2; exit 1; }
url="postgres://postgres@${INV_PG_ADDR}/postgres?sslmode=disable"
"${CARGO_TARGET_DIR:-target}/debug/space-nextest-postgres-setup" "$url"
printf 'SPACE_TEST_POSTGRES_ADMIN_URL=%s\n' "$url" >>"$NEXTEST_ENV"
```

Each run was `docker run --rm -v "$PWD:/workspace" -w /workspace <capability flags>
space-ci:local-user bash inner.sh test`. `inner.sh` exports the environment listed
under [Method](#method-and-environment), runs CI's nextest command, and appends any
`-E` filter. The capability flags for each run are in the tables above. Failures were
read from nextest's final summary, using each test's last attempt and its first panic
line.

## Decision summary (repeated)

1. 39 of 8871 tests need more than an ordinary user: 23 root, 4 SYS_ADMIN/devices/seccomp,
   12 Docker socket. Two more silently skip without root. No test needs host
   networking; only the Redis setup script does.
2. Add a `privileged` nextest test group and run `not group(privileged)` unprivileged
   and `group(privileged)` privileged. Measured: 8832/8832 and 39/39.
3. Use one build and two `docker run` steps on the same runner. The privileged step uses
   nextest reuse-build. Estimated cost is about +20–40 s per CI run.
4. Fix instead of moving:
   - the db-migrate baseline filter gap;
   - `sudo` escalation in the build-mgr copy test;
   - the `SO_MARK` dependency in the net-gateway exemption test;
   - the fixed `/space/cache` and `/space/run/net-gateway` paths in tests;
   - silent root skips, which should fail closed in CI.
5. The unprivileged image needs a declared non-root user, matching groups, and a
   writable `CARGO_HOME`.

## Ranked actions (repeated)

1. Add the `privileged` group. Run the default job non-root, without `--privileged`,
   host network or the socket, using `-E 'not group(privileged)'`.
2. Give the setup scripts a runner-provided-server path, and stop publishing Redis on
   `127.0.0.1` only.
3. Add `postgres_baseline_tests::` to the Postgres setup filter.
4. Add a non-root `USER` with a passwd entry and a writable `CARGO_HOME`. Never use an
   undeclared `--group-add`.
5. Privileged step: root, `--privileged`, the socket, no host network, nextest
   reuse-build.
6. Fix the host-safety bugs: the `/space/cache` tests, the `sudo` copy test, and the
   `/space/run/net-gateway` test. Keep the loop-device test in the privileged step only.
7. Make root-only tests fail closed in CI.
8. Remove the net-gateway exemption test's CAP_NET_RAW dependency.
9. Run the btrfs opt-in tests in the privileged step (`btrfs-progs` plus a loop btrfs).
10. Move the M1 SDK check out of the unprivileged container.
