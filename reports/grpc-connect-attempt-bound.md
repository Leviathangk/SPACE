# Bound every whole TCP gRPC connect attempt in the shared connect helper

- Base: `main` at `bea0d3f2bc8d82ab11ccc1e7017b899bd3b3d1e2`
- Change: `ac/grpc-connect-attempt-bound` at `8fe3a6f727b949effbc827691a5fccbd35c776db` (one commit)
- Box: Linux x86_64, 16 vCPU, KVM; Rust 1.98.0 (pinned), protoc 35.0, nextest 0.9.133

## Problem (verified against the vendored sources)

- `retry_tcp_connect` checks `max_elapsed` only between attempts; nothing bounded one attempt.
- tonic 0.14.6 `Endpoint::connect()` builds an `HttpConnector` with `set_connect_timeout`
  (`endpoint.rs` `http_connector`). In hyper-util 0.1.20 `HttpConnector::call_async` resolves
  the name first (`resolve(...)`, unbounded) and only then applies the timeout to the TCP
  connects (split across the resolved addresses). TLS runs after the inner connector in tonic
  `channel/service/connector.rs`, unbounded unless the `ClientTlsConfig` sets a timeout. The
  HTTP/2 handshake follows in `MakeSendRequestService::call`.
- So a peer that accepts TCP and never answers the ClientHello, or a stalled lookup, held
  `connect_tcp_channel_with_retry` indefinitely; the build-manager client sets no
  `connect_timeout`, so even its TCP connect was unbounded.

## Design

`retry_tcp_connect` now takes an `attempt_cap` and runs each attempt under
`tokio::time::timeout(attempt_cap, …)`, the way `retry_uds_connect` already did. An attempt that
hits the cap becomes `TcpConnectError::AttemptTimeout { service_name, target, cap }`, flows
through the same `record_connect_error` span status, `client_retry` metric, debug retry log and
exhaustion warning as any failed attempt, and is redialed while the budget remains. Budgets and
backoff are untouched.

- **Cap** = the endpoint's own `connect_timeout` (`Endpoint::get_connect_timeout()`, 3 s when
  unset) + `CLIENT_TLS_HANDSHAKE_TIMEOUT` (3 s). Every current caller dials with 3 s or nothing,
  so the cap is 6 s for all of them. Deriving the cap from the caller's own dial bound keeps it
  above that bound for any future caller with a longer `connect_timeout`, so no field on
  `GrpcRetryPolicy` was needed. (Adding a field would also have broken about 20 full struct
  literals across crates and added a meaningless setting to RPC retry policies.)
- **Worst case total** = `max_elapsed` + one cap, because the loop starts a new attempt only
  while `elapsed < max_elapsed` and that attempt then runs at most one cap: 10 s + 6 s = 16 s for
  `DEFAULT_TCP_GRPC_CONNECT_RETRY_POLICY` with a 3 s dial. Both helpers' doc comments say this.
- **Error type**: `connect_tcp_channel_with_retry` now returns `TcpConnectError`
  (`Transport(#[from] tonic::transport::Error)` marked `#[error(transparent)]`, plus
  `AttemptTimeout`). tonic offers no public way to construct a `transport::Error`, so the cap
  needs its own variant. Display and `source()` of transport failures are unchanged.
- **Callers that inspect connect errors** (all checked):
  - api (`connect_worker`, `connect_worker_internal`, `connect_builder`, `builder_node_client_at`):
    they only format with `{err}`, so the text is unchanged for transport errors.
  - credmgr client `ConnectError`: a hand-written `From<TcpConnectError>` maps `Transport` to the
    existing `ConnectError::Transport`, so its endpoint test that matches `Transport(_)` with an
    io `TimedOut` source still passes, and `AttemptTimeout` to the new `ConnectError::TcpConnect`.
  - build-mgr `object_store::is_retriable` matches credmgr `ConnectError` exhaustively; I added
    `TcpConnect(_) => true`, since an abandoned connect attempt is transient like `Transport`.
  - build-mgr client `ConnectError`: same `From` mapping (keeps `Transport`, adds `TcpConnect`).
    Its callers use `?` into `anyhow` or only format the error.
- 3 s seconds of allowance is generous next to the time a normal attempt spends outside the TCP
  dial: an in-cluster lookup takes milliseconds, a TLS 1.3 handshake needs one round trip, and
  the HTTP/2 client handshake only writes the preface to a fresh socket. With a 10 s budget, a
  6 s cap still leaves room for a full redial after a stalled first attempt.

### Caller audit

| Caller (file) | Helper | `connect_timeout` | TLS | Dials | Cap / worst total |
|---|---|---|---|---|---|
| api `connect_worker` (`substrate/mod.rs` ~3917), from `node_api_endpoint` via `POD_WORKER_CONNECT_TIMEOUT`, `DIRECT_NODE_CONNECT_TIMEOUT` or inline 3 s | `_with_retry` | 3 s | internal TLS (https by default; Kind provisions it) | Kubernetes: pod IP literal; direct: node address from the DB | 6 s / 16 s |
| api `connect_worker_internal` (~3934) | `_with_retry` | 3 s | internal TLS | same as above | 6 s / 16 s |
| api `connect_builder` (~3954) | `_with_retry` | 3 s | internal TLS | direct builder address | 6 s / 16 s |
| api `builder_node_client_at` (`kubernetes/pod_ops/worker_clients.rs`) | `_with_retry` | 3 s (`POD_WORKER_CONNECT_TIMEOUT`) | internal TLS | DNS Service name `space-builder-node-api:9800` | 6 s / 16 s |
| build-mgr client `connect_channel` (`build/mgr/client/src/lib.rs`) | `_with_retry` | **none** (3 s assumed for the cap) | internal TLS | build-manager DNS name | 6 s / 16 s (TCP was unbounded before) |
| credmgr client `Tcp` (`credential/mgr/client/src/endpoint.rs`) | `_with_retry` | 3 s (#309, kept) | no | configured URI (Service DNS name) | 6 s / 16 s |
| credmgr client `Tls` | `_with_retry` | 3 s (#309, kept) | server TLS, now `.timeout(3 s)` | configured URI (Service DNS name) | 6 s / 16 s |
| api `WorkerChannels::dial` (`substrate/worker_channels.rs`) | `_with_retrying_redials` | `attempt_timeout` = 3 s (`DIRECT_NODE_CONNECT_TIMEOUT`) | internal TLS | direct node address | per attempt 6 s; whole dial/redial still `max_elapsed + attempt_timeout` = 13 s (unchanged) |

A workspace grep finds only one `connect_tcp_channel_with_retrying_redials` call site
(`worker_channels.rs`). Its users reach it through `WorkerChannels`, which `direct.rs` builds,
and through `worker_channels/tests.rs`. I found no second production caller. Callers outside the helpers (for example
`node_maintenance.rs`, which calls `endpoint.connect()` directly) do not get the attempt cap.

## TLS handshake timeout decision

- `ClientTlsConfig::timeout(Duration)` exists in tonic 0.14.6 (`channel/tls.rs`). The
  connector stores it and wraps `RustlsConnector::connect` in `tokio::time::timeout`
  (`service/tls.rs` `TlsConnector::connect`), failing with `TlsError::HandshakeTimeout`
  ("TLS handshake timeout."). It covers only the TLS handshake. It runs on every connect the
  channel makes, initial dial and reconnects alike, because the TLS connector sits inside the
  channel's `Connector` service.
- New `pub const CLIENT_TLS_HANDSHAKE_TIMEOUT = 3 s` in `pplx-space-common-grpc`. It is set on:
  - the credmgr client `Tls` config (`client_tls_config`);
  - `internal_transport::ClientTransport`'s TLS config. That one place configures TLS for every
    internal-RPC client: the node-api and build-mgr callers above, `WorkerChannels`, and the
    build-mgr lazy channel. It also covers internal clients outside the TCP helpers (data plane,
    node-ctl, transfer, activation, cluster discovery).
  
  Why the shared config rather than per caller: tonic's `Endpoint` has no way to change a TLS
  config once applied, so the node-api and build-mgr callers can only get the bound there. The
  listener side already bounds handshakes at 10 s for resource reasons. `docs/internal-rpc-tls.md`
  now states the client bound.
- Consistency with the cap: cap = dial + TLS timeout. A handshake that fits its own bound
  always fits the attempt cap, and in the first connect whichever fires first ends the attempt.
- Not changed (follow-up): the credmgr server's intermediate key-broker client
  (`credential/mgr/server/src/run.rs`, `connect_lazy` with a bare `ClientTlsConfig`) is not a
  TCP-helper caller. It has the same unbounded-handshake gap on its lazy reconnects.

## Redials helper decision

`connect_tcp_channel_with_retrying_redials` shares `retry_tcp_connect`, so I included it with
a small change in the same file:

- Each retried attempt (DNS + TCP via `HttpConnector`) is now capped at
  `attempt_timeout + CLIENT_TLS_HANDSHAKE_TIMEOUT`. A redial whose lookup stalls is now abandoned
  and redialed. Before, it held that dial until the endpoint's outer bound.
- `RetryingTcpConnector::Error` is now a boxed error, so the cap's `TcpConnectError` can travel
  through tonic. tonic wraps connector errors either way, and the helper's return type,
  `tonic::transport::Error`, is unchanged.
- TLS for this helper is layered by tonic after the retried attempt succeeds, so a TLS stall is
  not redialed inside the same dial. It was already bounded by the endpoint's whole-dial
  `connect_timeout(max_elapsed + attempt_timeout)`: tonic's `connect_with_connector` wraps the
  whole connector, TLS included, in `hyper_timeout::TimeoutConnector`. It is now also bounded per
  handshake (3 s) through `ClientTransport`. **Follow-up:** retrying a TLS stall within one dial
  would mean moving TLS inside `RetryingTcpConnector`. That restructures the connector, so I left
  it out.
- Other follow-up: reconnects of channels returned by `connect_tcp_channel_with_retry` still
  resolve DNS without a bound, since tonic's reconnect path bounds only TCP and now TLS. Closing
  that would need a custom connector for those callers.

## Tests (`crates/libs/common/grpc/src/connect/tests.rs`)

1. `a_stalled_tcp_attempt_is_abandoned_at_its_cap_and_redialed` (paused time): through
   `retry_tcp_connect`'s closure, attempt 1 never resolves and attempt 2 connects. The test
   asserts the call returns attempt 2's value and that elapsed time is exactly cap + the first
   backoff.
2. `stalled_tcp_attempts_end_within_the_budget_and_one_attempt_cap` (paused time): no attempt
   ever resolves. The test asserts the result is `AttemptTimeout` carrying that cap, that more
   than one attempt ran, and that `max_elapsed < elapsed ≤ max_elapsed + cap`.
3. `a_tls_peer_that_never_answers_fails_the_connect_at_the_handshake_bound`
   (`#[cfg(feature = "internal-tls")]`, real time, no sleeps): a loopback listener accepts and
   holds every socket. The endpoint is built through `internal_transport::ClientTransport` (the
   product TLS config, with an rcgen CA) and `connect_tcp_channel_with_retry` runs with
   `GrpcRetryPolicy::none()` and a 1 s dial. The test asserts a `Transport` error whose source
   chain contains tonic's "TLS handshake timeout", and
   `CLIENT_TLS_HANDSHAKE_TIMEOUT ≤ elapsed < attempt cap`. It uses real time because a paused
   clock could auto-advance to the attempt cap before the runtime observes the loopback dial's
   readiness, which would make the test flaky.

No test asserts a configured value: the bounds are compared with the constants and functions
they come from.

Mutation results (each mutation applied alone, then reverted):

| Mutation | Test 1 | Test 2 | Test 3 |
|---|---|---|---|
| Remove the per-attempt `tokio::time::timeout` in `retry_tcp_connect` | FAIL (outer timeout: "a stalled attempt must not hold the connect") | FAIL ("stalled attempts must not bypass the attempt cap") | PASS (the TLS bound still fires; expected) |
| Remove `.timeout(CLIENT_TLS_HANDSHAKE_TIMEOUT)` from `ClientTransport` | PASS | PASS | FAIL at 4.0 s: the attempt cap fired instead, so the error is `AttemptTimeout` |
| Remove both | FAIL | FAIL | FAIL at 8.0 s on the outer bound (the connect hangs) |

Stability: tests 1 and 2 passed 300/300 (`--stress-count 300`, 1.2 s total). Test 3 passed
300/300 (908.8 s; every iteration took 3.02–3.04 s).

## Validation

### Step 1 — fmt, clippy, nextest

- `cargo fmt --all -- --check`: clean. `scripts/check-rust-error-policy.sh`: clean.
- Workspace clippy with CI's flags (`--workspace --locked --all-targets --all-features -D warnings`)
  plus CI's three feature-isolation runs (observability otel, common-svc no-default,
  feature-flags no-default): all clean, 0 warnings.
- nextest (`SPACE_CI_REQUIRE_TEST_DEPS=1`, repo `.config/nextest.toml` with its Postgres and Redis
  setup scripts on local Docker) for `pplx-space-common-grpc` and all 70 workspace crates that
  depend on it (transitive reverse dependencies from `cargo metadata`), plus
  `pplx-space-db-test-fixtures`, which provides the Postgres setup binary: **7686 run, 7652
  passed, 34 failed, 31 skipped (ignored by default).**
  - 33 failures in `pplx-space-spaced-guest-server` (lib and `it`/`worker_subprocess` binaries)
    and `pplx-space-spaced-host-cli::output`: process start fails with
    `Permission denied (os error 13)` or `AssumeIdentity … Operation not permitted`.
    `worker_subprocess` is in the root-only `privileged` group. Base `bea0d3f` fails the same 33
    on this box (221 passed / 33 failed for those two crates) → **PRE-EXISTING / ENVIRONMENTAL**
    (this container user cannot spawn or switch identity as these tests need).
  - 1 failure, `pplx-space-net-gateway tcp_proxy::tests::tls_interception::interception_exemptions_relay_original_tls_only_after_admission`
    (`exempt host: Elapsed`): fails 3/3 with the change and also on base → **PRE-EXISTING**. That
    crate does not use the changed helpers.
  - No CHANGE-INDUCED failures. I did not run the `privileged` group as root.
- Common-grpc alone with `--features internal-tls`: 76/76 passed.

### Step 2 — Kind

- Disk before bring-up: Docker root had 48.4 GiB free (`df -Pk /` in a container), above the
  20 GiB floor, so I removed no images. The owner's removal pre-authorization was not needed.
  30.2 GiB were free after the run.
- Prerequisites installed the CI way: `install-prereqs.sh` (kind 0.30.0, kubectl 1.32.0,
  helm 3.16.4, tilt 0.37.5, aws CLI), plus `space_smoke_prepare_encrypted_targets` for the
  device-mapper targets. `/dev/kvm` was already readable and writable through the `kvm` group,
  so I skipped CI's udev chmod.
- Bring-up: `smoke-kind-e2e.sh` with `DATA_PLANE_URL=http://127.0.0.1:8090`,
  `SPACE_KIND_OBSERVABILITY=off`, `SPACE_KIND_TELEMETRY_MODE=datadog`, `BOOTSTRAP_TIMEOUT=1800`.
  It ran from a cold start (fresh cluster, first Rust build from this worktree; the synced
  `rust-src` contains the change) and finished in 12 min. The 7-step round trip passed.
- Typed suite on the same cluster: `SKIP_BOOTSTRAP=1`, CI's shard env
  (`SPACE_SMOKE_KIND_ADMIN_TOKEN_FILE` from an admin session, `SPACE_SMOKE_KIND_COLD_RECEIPTS`,
  data-plane health gate), `smoke-kind-typed.sh all` with **no shard argument**. "Full suite"
  means the whole inventory, and each CI shard needs its own cluster. Run time 47 min.
  - ECW surface: **76 passed, 0 failed**.
  - Native surface: 22 passed, 1 INCONCLUSIVE: `native_contract.zero_vcpu_admission`, "selector
    for space-cluster-controller includes an unowned pod" during "restore and verify native
    recovery deployments", a harness precondition after the previous recovery restarts. I reran
    it once with the printed rerun command: **PASS** → ENVIRONMENTAL (transient harness state),
    not change-induced.
- Bound-firing check: none of the 51 `space` pods' logs or the aggregated Tilt log for the whole
  run contain "did not finish within", "TLS handshake timeout" or "exhausted TCP gRPC connect
  retry budget". Neither new bound cut a normal connect anywhere in the suite.
- Cleanup: `infra/space/kind/scripts/teardown.sh` stopped Tilt and the port-forwards and deleted
  cluster `space-local`; no kind clusters, Tilt processes or containers remain. Docker images and
  caches were not pruned.

## Decisions recorded

1. Cap derived from the endpoint's `connect_timeout` + 3 s instead of a policy field: it fits
   every caller and cannot fall below a future caller's dial bound.
2. New `TcpConnectError` with a transparent `Transport` variant, since tonic offers no way to
   construct a `transport::Error`; credmgr and build-mgr keep `Transport` for transport errors.
3. TLS timeout of 3 s set in the shared internal-transport config as well as on credmgr `Tls`,
   which reaches internal clients beyond the helper callers. The key-broker client is a follow-up.
4. Redials helper: included, since a small change closes its DNS gap. TLS-stall redial within a
   dial is a follow-up.
5. The TLS test uses real time, to avoid races between the paused clock and loopback readiness.
6. Typed Kind suite run unsharded on the one cluster.
7. No `Co-Authored-By` trailer, per the task's push rule.

## Results

| Check | Result |
|---|---|
| fmt + error-policy | PASS |
| Workspace clippy (CI flags) + 3 isolation runs | PASS (0 warnings) |
| nextest common-grpc + 70 dependents | 7652/7686 pass; 34 PRE-EXISTING/ENVIRONMENTAL (same on base); 0 CHANGE-INDUCED |
| New tests: mutations | each new bound's removal fails its test(s) |
| New tests: 300× stress | 300/300 each |
| Kind bring-up + e2e smoke | PASS |
| Typed Kind, ECW surface | 76/76 PASS |
| Typed Kind, native surface | 22 PASS + 1 INCONCLUSIVE → rerun PASS (ENVIRONMENTAL) |
| Cap or TLS timeout fired in Kind | never |
| Cleanup | cluster, Tilt and port-forwards gone; no images pruned |

**Verdict: the change is ready.** Every TCP connect attempt is now bounded as a whole
(cap 6 s; worst case 16 s for every current caller). The TLS handshake is bounded at 3 s on every
dial and reconnect of the credmgr and internal-transport clients. Errors stay compatible, and
no validation failure was caused by the change.
