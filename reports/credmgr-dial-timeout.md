# Bound each credential-manager client dial: report

Status: **PARTIAL, blocked on Docker-root disk space.** The change is landed
and pushed. Steps 1 and 2 are done. Step 3 has 1 of 3 runs, and step 4 has not
run: after the step-3 run-2 teardown, Docker-root free space was 14.3 GiB,
below the 20 GiB floor the task sets. No images, volumes or build caches were
pruned.

- Change: `refs/heads/ac/credmgr-dial-timeout` at
  `2b25606a36a819317402af4159abbaea23a1a41f`, one commit on `main` at
  `a8b5df675d8526bedbc2a1939b1748bafa317ef3`.
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
  `max_elapsed <= elapsed <= max_elapsed + CONNECT_TIMEOUT` on Tokio's clock.
  An outer `timeout(6 × budget)` turns an unbounded dial into a fast failure.
- It is `#[cfg(target_os = "linux")]`, because dropping SYNs on a full accept
  queue is Linux behaviour.

**It fails without the fix.** I removed the bound from each arm in turn and the
test failed each time: first `Tcp(..): an unanswered dial must be bounded`,
then the same for `Tls { .. }`. With the fix restored it passes.

**Flakiness.** 0 failures in 500 sequential runs and in 800 concurrent runs
(8 × 100).

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

**Run 1:** smoke rc 0 (round trip ok), wall time 395 s, which includes this
branch's first image builds. The authority Service was created at
16:02:03Z. The conntrack watcher found no pinned SYN, so the race was not hit.

| Pod (node) | Ready after the authority Service was created | Restarts | Last exit |
|---|---|---|---|
| space-credential-manager (worker) | 13 s | 0 | — |
| space-credential-manager (worker2) | 13 s | 0 | — |
| space-builder-credential-manager (worker4) | 4 s | 0 | — |
| space-builder-credential-manager (worker5) | 5 s | 0 | — |

**Runs 2 and 3: BLOCKED.** After the run-2 teardown, Docker-root free space
read 14.3 GiB, below the 20 GiB floor. The bring-up was not started.

### 4. Full typed Kind smoke suite

**Not run.** It needs a new bring-up, which the same disk floor blocks.

## Disk: why the floor was reached

Docker-root free space is the separate Docker volume, read with
`docker run --rm --entrypoint df space-worker:<tag> -Pk /`:

| Point | Free |
|---|---|
| Before run 1 | 28.3 GiB |
| Run 1 cluster up, after the fix repro | 10.9 GiB |
| After a Tilt no-op rebuild | 8.5 GiB |
| After loading the base image | 8.3 GiB |
| After teardown | **14.3 GiB** |

- `docker system df` after teardown: images 25.5 GB (17.3 GB reclaimable),
  build cache 13.9 GB (4.6 GB reclaimable).
- This task's own builds added a new `space-rust-chef-base:local` (3.4 GB), the
  Rust artifact image, and a new Tilt image set.
- **The Tilt no-op rebuild was my mistake.** I first started the base swap by
  reverting the client crate in the Tilt-watched worktree. When I saw 10.9 GiB
  free, I restored the source at once, and from then on built the base binary
  outside Tilt (the smaller-footprint method above). Tilt still rebuilt.
  The images came out byte-identical (same image ID `06abdd149dc2`, no
  rollout), but the rebuild used about 2.4 GiB, which looks like build cache.
- To unblock, the operator has to free Docker-root space or authorize removing
  this task's images and build cache. Then runs 2–3 and step 4 can run as above.

## Cleanup done

- Kind cluster deleted with `teardown.sh`, which also stops Tilt. No Tilt or
  `kubectl port-forward` processes remain.
- The task's base source and target directories are deleted, and so is the
  `space-worker:credmgr-base-a8b5df6` image I built. Nothing else was removed.
- The task checkout is on `ac/credmgr-dial-timeout` at its pushed HEAD,
  `2b25606`, and is clean.

## Follow-ups (not in this change)

1. **Agent startup exits on upstream failure.** `run_agent` exits 1
   (`AgentUpstreamConnect`) when the first upstream connect fails, and kubelet
   then restarts it with back-off. With this bound a pinned dial costs about
   3 s instead of the whole budget, but a longer outage still turns into
   restarts. Keeping the agent retrying, or connecting lazily, is a separate
   change.
2. **The TLS handshake is not bounded per dial.** `connect_timeout` covers
   TCP only. A peer that accepts TCP but stalls TLS would still hold a dial
   past the budget. That is not the observed mechanism, but a per-attempt
   timeout around the whole dial would close it.
3. **Pre-existing net-gateway test failure.**
   `tls_interception::interception_exemptions_relay_original_tls_only_after_admission`
   fails on base in this environment and should be looked at separately.

## Results

| Step | Result |
|---|---|
| A/B | **A**: `connect_from_env` callers use short-lived per-operation clients, so B's first condition fails |
| Unit test | Added. Deterministic (paused time, loopback full accept queue), covers both arms, fails without the fix, 0/1300 flaky |
| 1. fmt / clippy / nextest (20 pkgs) | pass / pass / 5662 pass, 19 ENVIRONMENTAL (root-only ignored tests), 1 PRE-EXISTING (fails on base) |
| 2. Pinned-SYN repro | fix **7.1 s, 6.5 s, 0 restarts** vs base **146.5 s, 1 restart** (`os error 110`); PREROUTING clean |
| 3. Natural bring-ups | run 1 **pass** (max 13 s after the Service, 0 restarts, race not hit); runs 2–3 **BLOCKED** (14.3 GiB < 20 GiB) |
| 4. Typed Kind suite | **not run** (same disk block) |

**Verdict: PARTIAL.** The change is landed and pushed. Unit tests and the
deterministic Kind repro show that a pinned SYN now costs one 3 s dial: a
worker credential-manager pod is Ready in about 7 s with no restart, instead of
about 147 s with one restart. Natural bring-ups 2–3 and the full typed suite
are blocked by the 20 GiB Docker-root floor and need operator-freed disk to
finish.
