# Bound each credential-manager client dial: report

Status: **COMPLETE.** All four validation steps have run.

- **Code and tests:** the change and the follow-up test revision are pushed.
- **Done:** steps 1–2, step 3 (three natural bring-ups, all passing; run 3
  hit the race), and step 4 (full typed Kind suite: 98 passed and 1 failed;
  that one passed on rerun and is classified PRE-EXISTING).
- **Disk history:** the 20 GiB Docker-root floor blocked the work twice. The
  box owner authorized two cleanups (details in the disk section):
  1. `docker builder prune -af` and `docker image prune -f`;
  2. removing superseded `space-*` `tilt-*` image tags, then
     `docker image prune -f`.
  Build cache (after the first cleanup) and volumes were not touched again.
- **Change:** `refs/heads/ac/credmgr-dial-timeout` at
  `270775ae0ec709c687eb517fa74f3f359e4cb71a`, two commits on `main` at
  `a8b5df675d8526bedbc2a1939b1748bafa317ef3`:
  - `2b25606` adds the bound and the test.
  - `270775a` revises the test's lower bound (review item 1). It is test-only,
    so the Kind binaries are the same as for `2b25606`.
- No pull request was opened, as instructed.

## A/B decision: A

I used **A**, the measured change: a named 3 s constant and `.connect_timeout(...)`
on the `Tcp` and `Tls` endpoints in `CredmgrEndpoint::connect`. B needs all four
of its conditions to hold. The first one fails:

1. **Is the channel long-lived and shared by its callers? No.**
   `CredmgrEndpoint::connect` has these callers:
   - `run_agent` (credential/mgr/server/src/run.rs) keeps the upstream client
     for the life of the process.
   - sbx-mgr `host_control.rs` and `gateway_authority.rs` cache the client in a
     `OnceCell`, but always on the node-local UDS endpoint, which this change
     does not touch.
   - `pplx_space_credmgr_client::connect_from_env()` is called once per
     operation, and the client is dropped when it returns. Its users are
     vol-mgr `credentials.rs` (3 call sites), build-mgr `object_store.rs`,
     node-mgr `service/encrypted_volumes.rs` and volmgr-svc `credential_probe.rs`.
     For these callers, retrying redials on a kept channel buys nothing.
2. **Does B work for `Tls`? Yes.** In tonic 0.14.6,
   `Endpoint::connect_with_connector` wraps the custom connector in
   `self.connector(..)`, which runs the TLS handshake on top of the connected
   stream (`transport/channel/service/connector.rs`).
3. **Do B's redial retries stack with a per-RPC retry layer?** Partly. sbx-mgr
   wraps its credmgr calls in `retry_grpc_status_with_context`, but over UDS.
   I didn't check this further, because condition 1 already fails.
4. **Do all callers tolerate waiting through a redial?** Not established; it
   doesn't matter once condition 1 fails.

## Diff summary (3 files, +56/−2)

- `crates/libs/credential/mgr/client/src/endpoint.rs`:
  - New constant `CONNECT_TIMEOUT: Duration = 3 s`. Its doc comment explains
    why: the retry budget is only checked between dials, and a SYN pinned to a
    stale conntrack entry (sent to a ClusterIP before kube-proxy programmed
    it) would hold the first dial through about two minutes of retransmits.
    The bound covers the TCP connect, not the TLS handshake.
  - `.connect_timeout(CONNECT_TIMEOUT)` on the `Tcp` endpoint and on the `Tls`
    endpoint (before `.tls_config`).
  - No other callers changed. `run_agent`'s exit-on-failure startup, the
    Tiltfile and the charts are unchanged.
- `crates/libs/credential/mgr/client/src/endpoint/tests.rs`: one test (below).
  `270775a` changes its lower bound, as described in the test section.
- `crates/libs/credential/mgr/client/Cargo.toml`: dev-dependency
  `tokio = { features = ["macros", "rt", "test-util"] }`, which the paused-time
  test needs.
- The commit uses a one-sentence imperative subject with an explanatory body,
  and has no Co-Authored-By trailer.

## Is 3 s a sensible value?

**What the bound covers.** tonic 0.14.6 `Endpoint::connect()` passes
`connect_timeout` to `HttpConnector::set_connect_timeout`, so it covers the TCP
connect only. TLS runs afterwards in tonic's `Connector` and is not covered.
hyper-util splits the timeout across the resolved addresses. A single-stack
Service name resolves to one ClusterIP, so that address gets the full 3 s.

**How the authority is reached in Kind.** The node agents dial
`https://space-credmgr-authority:9810`:
- The URL is `node.credmgrAuthorityEndpoint` in
  `infra/space/kind/charts/space-node-service/values.yaml`. The
  `credmgrAuthority` helper renders it with the CA
  `/var/run/space-auth/credmgr-ca.crt`.
- That name is a ClusterIP Service (`space-credmgr-authority-local`) in front
  of a 2-replica Deployment on the infra node.
- The path is pod → kube-proxy DNAT → pod IP over the Kind Docker bridge.
- The certificate is a self-signed ECDSA P-256 server certificate from
  `internal-rpc-tls.sh`.

**How it is reached in the production-shaped config here.** This repository has
no production Helm chart for the node agent. The EKS Terraform module has one
clue: `credmgr_authority_tls.tf` issues the authority certificate for the
in-cluster Service DNS names only (`space-credmgr-authority`, `.space`,
`.space.svc`, `.space.svc.cluster.local`). So callers dial the ClusterIP
Service inside the VPC. `locals.tf` puts the authority on a dedicated
`cred-authority` node pool.

**Comparison.** A TCP handshake is one round trip: well under 1 ms on a Kind
bridge, and normally a few ms inside one VPC even across AZs. 3 s is about three
orders of magnitude above that. The TLS handshake isn't charged against the
bound, though even with it the total would stay far below 3 s. In-repo
precedent points the same way:
- The authority's own probes allow `grpc_health_probe -connect-timeout=2s`
  (readiness) and `3s` (liveness), and those include TLS
  (`space-credmgr-authority-local/templates/deployment.yaml`).
- The node-api activation client uses 3 s and the cluster-api client 5 s.

**Effect on the retry budget.** The budget is
`DEFAULT_GRPC_CONNECT_RETRY_POLICY` (10 s max elapsed, 100 ms→1 s backoff).
With a 3 s bound per dial there are about 4 dials (at roughly t = 0, 3.1, 6.3
and 9.7 s), and `connect` returns by about 12.7 s at worst.

## Test decision: one deterministic test added

`endpoint::tests::an_unanswered_dial_fails_within_the_connect_budget` pins this
contract: when a dial is never answered, `connect` keeps redialing until the
retry budget is spent, then returns an error. It overshoots by at most one
dial.

How it works:
- A loopback `TcpSocket` listens with `backlog = 0`, and one unaccepted
  connection fills its accept queue. Linux then drops the SYN of every further
  dial, which stands in for the pinned SYN: there are no blackhole addresses,
  routes or firewall rules, only loopback in the test's own namespace.
- It runs under `#[tokio::test(start_paused = true)]`, so no wall-clock sleeps.
  The test takes about 6 ms.
- It runs over both arms: `Tcp`, and `Tls` with an empty CA file, which is
  never read because TCP never completes.
- It asserts `Err(ConnectError::Transport)` and
  `CONNECT_TIMEOUT <= elapsed <= max_elapsed + CONNECT_TIMEOUT` on Tokio's
  clock. An outer `timeout(6 × budget)` turns an unbounded dial into a fast
  failure.
- **Lower bound, revised in `270775a`.** It was `elapsed >= max_elapsed`, which
  depends on the policy's backoff schedule: a later policy change could fail
  the test while every dial was still bounded. It is now
  `elapsed >= CONNECT_TIMEOUT`, meaning the dial really hung and the bound cut
  it, rather than failing fast for some other reason. The upper bound and the
  test comment were adjusted to match.
- It is `#[cfg(target_os = "linux")]`, because dropping SYNs on a full accept
  queue is Linux behaviour.

**It fails without the fix.** I removed the bound from each arm in turn and the
test failed each time: first `Tcp(..): an unanswered dial must be bounded`,
then the same for `Tls { .. }`. With the fix restored it passes.

**Flakiness.** 0 failures in 500 sequential runs and in 800 concurrent runs
(8 × 100).

**Re-checked after `270775a`:**
- `cargo fmt --all -- --check` passes.
- `CARGO_TARGET_DIR=target/buildkite-lint cargo clippy --locked -p pplx-space-credmgr-client --all-targets --all-features -- -D warnings`
  passes.
- `cargo nextest run --locked -p pplx-space-credmgr-client --all-targets --all-features`
  passes 28/28.
- With the bound removed from `Tcp`, then from `Tls`, the test fails each time
  with "an unanswered dial must be bounded", and passes again once restored.
- 0 failures in 300 more sequential runs.

## Validation

### 1. Rust checks

| Check | Command | Result |
|---|---|---|
| Format | `cargo fmt --all -- --check` | pass |
| Clippy (CI flags) | `CARGO_TARGET_DIR=target/buildkite-lint cargo clippy --workspace --locked --all-targets --all-features -- -D warnings` | pass |
| Nextest, the touched crate plus all 19 reverse dependencies | `SPACE_CI_REQUIRE_TEST_DEPS=1 SPACE_TEST_DEP_SKIP_LOG=<file> cargo nextest run --locked -p <20 pkgs> --all-targets --all-features --run-ignored all --no-fail-fast` | 5682 run, 5662 passed, 20 failed, 0 skipped. The skip log stayed empty. |
| Nextest, credmgr-client after the final source | `cargo nextest run --locked -p pplx-space-credmgr-client --all-targets --all-features` | 28/28 pass |

The 20 packages were found with `cargo tree --workspace -i
pplx-space-credmgr-client -e normal,dev`:
- api, api-svc
- build-mgr, build-mgr-cli, build-mgr-server, build-mgr-svc
- cluster-controller, cluster-controller-server
- credmgr, credmgr-client, credmgr-svc
- net-gateway, net-gateway-svc, netgtw-bench
- node-mgr, node-mgr-svc
- sbx-mgr, sbxmgr-svc
- volmgr, volmgr-svc

How the 20 nextest failures classify:
- **19 ENVIRONMENTAL.** They are all `#[ignore = "requires root …"]` tests,
  which `--run-ignored all` selects, and this box runs them as an unprivileged
  user:
  - 16 in volmgr-svc `identity_handoff::tests::*`
  - credmgr `run::volume_socket::tests::preflight_checks_real_permissions_without_replacing_live_socket`
  - node-mgr `clone_tree::tests::private_clone_blocks_foreign_uid_grafts_and_shared_clone_is_held`
  - sbx-mgr `transfer_directory::tests::distinct_service_uids_can_exchange_downloads`
- **1 PRE-EXISTING.** net-gateway
  `tcp_proxy::tests::tls_interception::interception_exemptions_relay_original_tls_only_after_admission`
  fails with `exempt host: Elapsed(())`. It failed 3/3 times on the fix and
  1/1 on base (client crate swapped back to base in place), and it doesn't use
  the credmgr client.

### 2. Deterministic pinned-SYN repro, base vs fix (Kind)

**Setup.** This ran on the run-1 cluster (below), on node `space-local-worker`.
The authority ClusterIP was `10.96.217.164`. For each case the procedure was:
1. Insert the rule in the node container:
   `iptables -t nat -I PREROUTING -d <ClusterIP> -p tcp --dport 9810 -j ACCEPT`.
2. Delete the node's credential-manager pod.
3. Poll `conntrack -L -p tcp --dport 9810 -d <ClusterIP>` until a
   `SYN_SENT … [UNREPLIED] src=<ClusterIP> dst=<node IP>` entry appears (not
   NATed: the reply source is the ClusterIP itself).
4. Delete the rule.
5. Poll the new pod's Ready condition every 0.2 s.

A captured pinned entry:
`SYN_SENT src=10.244.3.6 dst=10.96.217.164 sport=57322 dport=9810 [UNREPLIED] src=10.96.217.164 dst=172.18.0.4`.

**Images.**
- *Fix*: the image Tilt built from this branch (`space-worker:tilt-06abdd…`).
- *Base*: I exported `a8b5df6` with `git archive` and compiled it in the same
  builder image (`space-rust-chef-base:local`), with the same cargo features
  and bin list as `docker/Dockerfile.rust-build-kind`. I then built a one-layer
  image that replaces only `/opt/space/bin/space-credmgr-svc` in the fix image.
  - The base binary's SHA-256 starts `4870f088…`. The fix binary, which matches
    the deployed one, starts `52205001…`.
  - The image was loaded only into the two worker-pool nodes. I switched the
    `space-credential-manager` DaemonSet to it, waited for the rollout, ran the
    repro, then switched back.

| Case | Pinned SYN seen | Ready after pod created | Restarts | Last exit |
|---|---|---|---|---|
| fix | 1.6 s after delete | **7.1 s** | 0 | — |
| fix (repeat, after switching back) | 1.8 s | **6.5 s** | 0 | — |
| base | 1.5 s | **146.5 s** | 1 | `Error/1`: `credential-manager transport failed … tcp connect error: Connection timed out (os error 110)` |

**Cleanup.** The rule was removed within 0.2 s of seeing the pinned SYN in
every case. Afterwards `iptables -t nat -S PREROUTING` had no `9810` rule on
any of the 6 node containers. The iptables commands ran only inside the Kind
node containers, never on the host.

### 3. Natural bring-ups with the fix

Each run: `infra/space/kind/scripts/teardown.sh`, then
`SPACE_KIND_OBSERVABILITY=off SPACE_KIND_TELEMETRY_MODE=datadog BOOTSTRAP_TIMEOUT=1800 DATA_PLANE_URL=http://127.0.0.1:8090 bash scripts/tests/smoke-kind-e2e.sh`.
A watcher polled every node's conntrack for port-9810 `SYN_SENT`/`UNREPLIED`
entries every ~0.5 s for the whole bring-up.

Pass rule: no credential-manager pod Ready later than 60 s after the authority
Service was created.

| Run | HEAD | Smoke | Wall time | Authority Service created | Pods Ready after the Service (worker, worker2, builder worker4, builder worker5) | Restarts | Last exit | Pinned SYN (race hit) | Pass |
|---|---|---|---|---|---|---|---|---|---|
| 1 | `2b25606` | rc 0 | 395 s (first image builds) | 16:02:03Z | 13, 13, 4, 5 s | 0 ×4 | — | no | **pass** |
| 2 | `270775a` | rc 0 | 259 s (cold rebuild after the prune) | 16:40:38Z | 14, 13, 16, 11 s | 0 ×4 | — | no | **pass** |
| 3 | `270775a` | rc 0 | 114 s (images reused) | 16:48:44Z | 29, 8, 35, 5 s | 2, 1, 2, 1 | `Error/1` ×4 (see below) | **yes**: on worker, worker4 (twice) and worker5 | **pass** |

Runs 1 and 2 did not hit the race. **Run 3 did.** About 1.3 s after the
authority Service appeared, the watcher caught un-NATed
`SYN_SENT … [UNREPLIED] src=10.96.86.30 dst=<node IP>` entries:
- worker5 at +1.3 s (sport 57384);
- worker4 at +1.3 s (sport 40118), and again at +4.2 s (sport 40120), so
  kube-proxy on that node was still unprogrammed when the 3 s bound forced a
  redial;
- worker at +2.2 s (sport 38224).

In run 3 the agent pods started about 21–23 s before the Service existed. That
is why all four restarted. The cause was the agent's exit-on-failure startup
(follow-up 1), never a stuck dial. Each container's last exit, from the
previous-container logs:

| Node | Previous run (relative to the Service) | Exit cause | Current run started | Ready |
|---|---|---|---|---|
| worker5 | −19 → −6 s | `dns error: failed to lookup address information` (Service not yet in DNS; 12 attempts, 9.1 s budget) | −5 s | +5 s, despite a pinned SYN at +1.3 s |
| worker2 | −21 → −8 s | `dns error` (13 attempts, 10.0 s) | −7 s | +8 s |
| worker4 | −6 → +11 s | `tcp connect error: deadline has elapsed` (the 3 s bound cutting the two pinned dials, then the budget ran out) | +32 s (crash-loop back-off) | +35 s |
| worker | −7 → +6 s | `tcp connect error: Connection refused` (Service had no ready authority endpoint yet; 12 attempts, 9.2 s) | +25 s | +29 s |

With the bound, the worst pod was Ready 35 s after the Service, within the
60 s rule. Without it, each pinned dial on worker, worker4 and worker5 would
have hung for about 130 s, as the base repro in step 2 shows.

### 4. Full typed Kind smoke suite

**What ran.** `bash scripts/tests/smoke-kind-typed.sh all`, unsharded, on run
3's cluster right after its baseline smoke, as CI does.

**Env**, from the kind-ci.yml shard step and job:
- `SKIP_BOOTSTRAP=1`, `SPACE_KIND_OBSERVABILITY=off`,
  `SPACE_KIND_TELEMETRY_MODE=datadog`;
- `SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE` (from the `root@local` session login);
- `SPACE_SMOKE_KIND_COLD_RECEIPTS` (a 0700 directory);
- `uv` from `ci/scripts/ensure-uv.sh`;
- the same data-plane `/healthz` wait.

**Not set:** `SPACE_CI_IMAGE_MIRROR=ghcr`, `SPACE_KIND_RUST_ARTIFACTS=prebuilt`
and the prebuilt image ID/SHA. They only affect the bootstrap or artifact
handoff, which `SKIP_BOOTSTRAP=1` skips, and the GHCR mirror needs CI
credentials.

**Prerequisites.** The host's `dm_crypt` and `dm_snapshot` modules were already
loaded for the BYOK scenarios.

**Result: rc 1, wall time 2946 s.**
- ECW API surface: **76 passed**, 0 warned, 0 failed, 0 inconclusive.
- Native API surface: **22 passed**, 0 warned, **1 failed**, 0 inconclusive.
- No scenario was quarantined.
- The OCI template-build scenario (`local.ecw_template_build_from_oci_boots`)
  **passed** on this box, along with `local.private_template_build_isolation`.
  So the known builder-storage admission failure did not occur, and there is
  no admission message to quote.

**The one failure**, `native.stopped_restart_preserves_rootfs` ("deleted stopped
sandbox was not reclaimed by node reconciliation"):
- **First attempt.** Both stop/restart cycles kept the rootfs payload. The last
  phase, "delete stopped sandbox and verify node cleanup", gave up after 188 s
  on `space-local-worker2`. node-manager's `cleanup_sandbox` ran and completed
  within a second of the delete. sandbox-manager's snapshot reaper then logged
  "unreadable subtree retained for a later sweep" (Permission denied) for that
  sandbox's directory under `/space/cache/firecracker/sandboxes/`, so the
  sandbox was not reclaimed within the scenario's wait.
- **Rerun once** (same env as the suite's native pass; the script's
  `prepare_native_kind` exports): **pass**. Same worker; the delete phase
  completed in 47.9 s.
- **Classification: PRE-EXISTING** (intermittent).
  - It is in the snapshot-reaper / node-reconciliation cleanup path, and it
    passed on rerun with the same binaries.
  - The change only touches the credmgr client's `Tcp`/`Tls` endpoints. The
    node services on that path (sandbox-manager, node-manager) reach credmgr
    through the node-local UDS endpoint, which the change does not touch.
  - It is not CHANGE-INDUCED, so the protocol needs no base comparison.
- **Agent stability.** The credential-manager agents' restart counts did not
  change during the 49-minute suite.

## Disk: why the floor was reached

Docker-root free space is the separate Docker volume, read with
`docker run --rm --entrypoint df space-worker:<tag> -Pk /`:

| Point | Free |
|---|---|
| Before run 1 | 28.3 GiB |
| Run 1 cluster up, after the fix repro | 10.9 GiB |
| After a Tilt no-op rebuild | 8.5 GiB |
| After loading the base image | 8.3 GiB |
| After the run-1 teardown | 14.3 GiB (run 2 first blocked) |
| After the owner-authorized `docker builder prune -af` and `docker image prune -f` | 25.1 GiB (builder prune reclaimed 13.9 GB; image prune reclaimed 0 B) |
| Before run 2 (after teardown) | 25.1 GiB |
| After the run-2 teardown | **13.7 GiB** (run 3 blocked) |

**Why it fell again.** With the build cache empty, run 2 was a cold build: a
new `space-rust-chef-base` (3.4 GB), artifact image and Tilt image set, and
11.4 GB of build cache. The older tagged image sets stayed, because `docker
image prune -f` removes only dangling images. About 2.4 GiB of the run-1 drop
was my mistake: I reverted the client crate in the Tilt-watched worktree for
the base swap. I restored it at once and built base outside Tilt, but Tilt
still rebuilt (identical images, no rollout).

**Second owner-authorized cleanup.** After the run-2 teardown, free space was
13.7 GiB. I listed the removal set first, then ran `docker rmi` without `-f`:
- **Removed:** 133 older `tilt-*` tags in the `space-*` repositories, across
  63 images. 61 images were deleted; the other 2 also carry a `:local` tag,
  which is kept.
- **Kept, per repository, the newest image's tags:**
  - the set run 2 built from this HEAD: `space-worker 493656df`,
    `space-api 4ca6f23f`, `space-ui 6e8727de`, `space-seed-publish fb21fad8`,
    `space-data-plane 971f5c57`, `space-cluster-controller b5dc3799`,
    `space-kind-key-broker aaa530c9`;
  - `space-node-stack f87d863d`, the newest of a repository this build does
    not use, kept under the "newest tag of each" rule.
- **Also kept:**
  - the single `space-rust-chef-base:local` and `space-rust-build-kind:local`
    images, both referenced by the current build, so neither was superseded;
  - all `:local` tags;
  - third-party and base images (kindest/node, the SPACE Kind node image, uv
    and so on);
  - the build cache and volumes.
- `docker image prune -f` reclaimed 0 B.
- Free space went from 13.7 to **26.3 GiB**.

| Point | Free |
|---|---|
| After the second cleanup | 26.3 GiB |
| Before run 3 (after teardown) | 26.3 GiB (images reused, so no rebuild) |
| During the typed suite | 18.9 GiB (cluster up) |
| After the final teardown | 26.3 GiB |

## Cleanup done

- Kind cluster deleted with `teardown.sh`, which also stops Tilt. No Tilt or
  `kubectl port-forward` processes remain.
- The task's base source and target directories are deleted, and so is the
  `space-worker:credmgr-base-a8b5df6` image I built. Nothing else was removed.
- After step 4, the run-3 cluster was deleted with `teardown.sh`. No Kind
  cluster, Tilt or port-forward remains.
- The task checkout is on `ac/credmgr-dial-timeout` at its pushed HEAD,
  `270775a`, and is clean.

## Follow-ups (not in this change)

1. **Agent startup exits on upstream failure.** `run_agent` exits 1
   (`AgentUpstreamConnect`) when the first upstream connect fails, and kubelet
   then restarts it with back-off. With this bound a pinned dial costs about
   3 s instead of the whole budget, but a longer outage still turns into
   restarts. Keeping the agent retrying, or connecting lazily, is a separate
   change. Run 3 shows the cost: agents started before the authority Service
   existed, exited on DNS or connection-refused errors, and two of them waited
   out crash-loop back-off until about 30 s after the Service.
2. **The TLS handshake is not bounded per dial.** `connect_timeout` covers
   TCP only. A peer that accepts TCP but stalls TLS would still hold a dial
   past the budget. That is not the observed mechanism, but a per-attempt
   timeout around the whole dial would close it.
3. **Pre-existing net-gateway test failure.**
   `tls_interception::interception_exemptions_relay_original_tls_only_after_admission`
   fails on base in this environment and should be looked at separately.
4. **Intermittent snapshot-reaper retention.** In
   `native.stopped_restart_preserves_rootfs`, the reaper hit "Permission
   denied" on a deleted stopped sandbox's snapshot subtree and retained it for
   a later sweep. The node-cleanup wait timed out once and passed on rerun.

## Results

| Step | Result |
|---|---|
| A/B | **A**: `connect_from_env` callers use short-lived per-operation clients, so B's first condition fails |
| Unit test | Added. Deterministic (paused time, loopback full accept queue), covers both arms. The lower bound is now `CONNECT_TIMEOUT` (`270775a`). Fails without the fix on each arm, re-checked after the revision. 0 flaky runs in 1600 |
| 1. fmt / clippy / nextest | Workspace clippy pass. Nextest over 20 packages: 5662 pass, 19 ENVIRONMENTAL (root-only ignored tests), 1 PRE-EXISTING (fails on base). After `270775a`: fmt pass, crate clippy pass, crate nextest 28/28 |
| 2. Pinned-SYN repro | Fix **7.1 s and 6.5 s, 0 restarts** vs base **146.5 s, 1 restart** (`os error 110`). PREROUTING clean |
| 3. Natural bring-ups | Run 1 **pass** (≤ 13 s after the Service, 0 restarts, race not hit). Run 2 **pass** (≤ 16 s, 0 restarts, race not hit). Run 3 **pass** (≤ 35 s; **race hit** on 3 nodes, and the bound cut the pinned dials; 1–2 restarts each, from DNS / connection-refused / exhausted-budget startup exits, not stuck dials) |
| 4. Typed Kind suite | ECW **76/76 pass**. Native **22/23 pass**. `native.stopped_restart_preserves_rootfs` failed once (snapshot-reaper retention during node cleanup), passed on rerun: **PRE-EXISTING**. The OCI template-build scenario passed |

**Verdict: PASS, with the pre-existing items noted.**
- The per-dial 3 s bound (A) is landed on `ac/credmgr-dial-timeout` at
  `270775a`.
- The deterministic Kind repro shows that a pinned SYN now costs one 3 s
  dial: Ready in about 7 s with no restart, instead of about 147 s with one
  restart.
- All three natural bring-ups meet the 60 s rule (worst 35 s). That includes
  run 3, which hit the race on three nodes, where base would have stalled each
  for about two minutes.
- The full typed suite shows no change-induced failure.
- The remaining restarts come from the agent's exit-on-failure startup, a
  separate follow-up.
