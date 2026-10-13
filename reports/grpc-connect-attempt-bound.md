# Bound every whole TCP gRPC connect attempt in the shared connect helper

- Base: `main` at `bea0d3f2bc8d82ab11ccc1e7017b899bd3b3d1e2`
- Change: `ac/grpc-connect-attempt-bound`, three commits:
  - `8fe3a6f` — round 0: per-attempt cap and TLS bounds
  - `d78eec5` — review round 1: each caller's attempt cap sized from its own TLS bound, and a 15 s internal-transport handshake bound derived from the server's deadline
  - `545a7fe` — review round 2: the internal transport passes the handshake bound it actually applies
- Box: Linux x86_64, 16 vCPU, KVM; Rust 1.98.0 (pinned), protoc 35.0, nextest 0.9.133

## Problem (verified against the vendored sources)

`retry_tcp_connect` checked `max_elapsed` only between attempts. Nothing bounded a single attempt.

In tonic 0.14.6, `Endpoint::connect()` applies `connect_timeout` to hyper-util 0.1.20's `HttpConnector`. That bounds only part of an attempt:
- Name resolution runs before the TCP connect, with no bound.
- TLS runs after it (`channel/service/connector.rs`), with no bound unless the `ClientTlsConfig` sets one.
- The HTTP/2 handshake then only writes the client preface.

The build-manager client sets no `connect_timeout`, so even its TCP connect had no bound.

## Design (current state, after review round 2)

### Per-attempt cap and worst case

`retry_tcp_connect` runs each attempt under `tokio::time::timeout(attempt_cap, …)`, as `retry_uds_connect` already did.
- An attempt that hits the cap fails as `TcpConnectError::AttemptTimeout`.
- It gets the same span status, `client_retry` metric, retry log and exhaustion warning as any failed attempt.
- It is redialed while the budget remains. Budgets and backoff are unchanged.

**cap = 3 s name-resolution allowance + endpoint dial timeout (3 s if unset) + the TLS handshake bound actually applied (0 for plaintext)**

**worst case = `max_elapsed` + one cap**

An attempt whose lookup and dial stay within their shares therefore fails with the TLS bound's own cause ("TLS handshake timeout") before `AttemptTimeout`.

| Caller class (10 s default budget, 3 s dial) | Cap | Worst case |
|---|---|---|
| Internal-RPC TLS (15 s handshake) | 21 s | 31 s |
| credmgr TLS (3 s handshake) | 9 s | 19 s |
| Plaintext (credmgr `Tcp`, internal RPC in insecure dev) | 6 s | 16 s |

Redials helper:
- Each retried attempt (DNS + TCP) is capped at 3 s + `attempt_timeout` = 6 s.
- Each whole dial or redial is bounded by `retrying_redial_bound` = `max_elapsed` + attempt cap + handshake bound. That is 31 s for internal TLS and 16 s for plaintext.
- `WorkerChannels::dial_budget` uses `internal_transport::retrying_redial_bound` (the same formula with `CLIENT_TLS_HANDSHAKE_TIMEOUT`), so it never cuts the 15 s handshake.

### TLS handshake bounds

| Client | Bound | Why |
|---|---|---|
| Internal RPC (`ClientTransport`) | `CLIENT_TLS_HANDSHAKE_TIMEOUT` = `SERVER_TLS_HANDSHAKE_TIMEOUT` (10 s, named, used by the listener) + 5 s = **15 s**, derived in code | The listener takes one of 64 handshake slots before it polls accept (`internal_transport/incoming.rs`), so under load a connected socket waits in the kernel accept queue. That wait counts against the client's timer but not the server's. No client bound can guarantee success while a listener stays saturated. |
| credmgr `Tls` | `TLS_HANDSHAKE_TIMEOUT` = **3 s**; #309's 3 s TCP `connect_timeout` kept | The credmgr listener hands accepted sockets straight to tonic TLS with no admission slots, so there is no queue wait to cover. |

`ClientTlsConfig::timeout` exists in tonic 0.14.6. It covers only the handshake, applies to every connect a channel makes (reconnects included), and fails with `TlsError::HandshakeTimeout`. `docs/internal-rpc-tls.md` states the 15 s bound and why it is derived that way.

### How the applied TLS bound reaches the cap (round 2: by construction)

tonic does not expose an endpoint's TLS config. The generic helpers therefore take `tls_handshake_timeout: Option<Duration>`, and their docs say internal-RPC callers must not call them directly.

**Internal RPC.**
- `ClientTransport` stores each TLS config in a private `ClientTls { config, handshake_timeout }`. `ClientTls::new` builds the config with `.timeout(handshake_timeout)` from that same value.
- `ClientTransport::connect_endpoint(endpoint)`, and the free `client_connect_endpoint(service, endpoint)` that mirrors `client_endpoint`, return a `ConnectEndpoint` with private fields: the transported endpoint, plus the bound of the TLS config applied to it (`None` for plaintext).
- `ClientTransport::endpoint` is now implemented through `connect_endpoint`, so there is a single code path.
- `ConnectEndpoint::connect_with_retry(service, target, policy)` and `ConnectEndpoint::connect_with_retrying_redials(service, attempt_timeout, policy)` pass that stored bound to the generic helpers.
- The scheme-based public `client_tls_handshake_timeout(&endpoint)` is removed. No internal-RPC caller states a bound any more.

**credmgr.** `client_tls_config` returns the `ClientTlsConfig` together with the 3 s bound it set, and the connect passes that value. The change was small, so I applied the same idea there.

**Correction to `d78eec5`.** That commit's body said "the compiler enforces it". The compiler only enforced that callers pass *some* `Option`. `545a7fe`'s body records the correction.

### Error compatibility

`connect_tcp_channel_with_retry` returns `TcpConnectError`: a transparent `Transport(tonic::transport::Error)` plus `AttemptTimeout`. tonic has no public way to construct its own transport error.
- api callers only format the error, so their text is unchanged.
- The credmgr and build-mgr `ConnectError` types keep mapping transport errors to `Transport`, and add `TcpConnect` for an abandoned attempt.
- build-mgr `object_store::is_retriable` classes `TcpConnect` as retriable.

## Caller audit (after round-2 migration)

| Caller | Path | Dial | TLS bound (source) | Dials to | Bounds |
|---|---|---|---|---|---|
| api `connect_worker`, `connect_worker_internal`, `connect_builder` (`substrate/mod.rs`) | `node_api_connect_endpoint` → `ConnectEndpoint::connect_with_retry` | 3 s | 15 s (transport) | pod IP literal or direct node address | cap 21 s, worst 31 s; reconnect handshakes 15 s |
| api `builder_node_client_at` (`kubernetes/pod_ops/worker_clients.rs`) | `ConnectEndpoint::connect_with_retry` | 3 s | 15 s (transport) | DNS name `space-builder-node-api:9800` | cap 21 s, worst 31 s |
| build-mgr client `connect_channel` | `client_connect_endpoint` → `connect_with_retry` | none (3 s assumed) | 15 s (transport) | build-manager DNS name | cap 21 s, worst 31 s (TCP had no bound before) |
| api `WorkerChannels::dial` (`worker_channels.rs`) | `ConnectEndpoint::connect_with_retrying_redials` | 3 s | 15 s (transport) | direct node address | attempt 6 s; whole dial or redial, and the shared-dial wait, 31 s |
| credmgr `Tcp` | generic `connect_tcp_channel_with_retry`, `None` | 3 s | none | Service DNS name | cap 6 s, worst 16 s |
| credmgr `Tls` | generic `connect_tcp_channel_with_retry` | 3 s | 3 s (`client_tls_config`) | Service DNS name | cap 9 s, worst 19 s; every handshake 3 s |
| api `pod_worker_channel` (`worker_clients.rs` ~29–47): wait-ready and wake (500 ms dial), teardown (3 s); node eligibility only resolves the endpoint these dial | `node_api_transport` + `Endpoint::connect()` (no helper) | 500 ms / 3 s | 15 s (transport) | pod IP literal | No attempt cap. A handshake that never completes is now cut at 15 s; before, only the callers' outer waits bounded it. |
| api admin `node_maintenance` (~535) | `client_endpoint` + `connect()` | 5 s | 15 s (transport) | operator-supplied endpoint | No attempt cap. Handshake bounded at 15 s; DNS still unbounded. |
| `NodeApiClient` (`node/api/client/src/lib.rs` ~288–304) | `GrpcClient` connect closure | 3 s | 15 s (transport) | node-api endpoint | `GrpcClient` already caps each whole connect attempt at 1 s, so the 15 s bound only matters on its cached channel's reconnects. That pre-existing 1 s cap undercuts a queued handshake too (follow-up). |

The only remaining callers of the generic helpers are credmgr (plaintext and its own 3 s TLS) and the helper's own tests.

## Tests (`crates/libs/common/grpc/src/connect/tests.rs`)

- **T1** `a_stalled_tcp_attempt_is_abandoned_at_its_cap_and_redialed` (paused time). Attempt 1 never resolves and attempt 2 connects; elapsed is exactly the cap plus the first backoff.
- **T2** `stalled_tcp_attempts_end_within_the_budget_and_one_attempt_cap` (paused time). The result is `AttemptTimeout`, more than one attempt ran, and `max_elapsed < elapsed ≤ max_elapsed + cap`.
- **T3** `a_tls_peer_that_never_answers_fails_the_connect_at_its_handshake_bound` (real time, about 4 s). It calls the generic helper against a loopback peer that accepts and holds sockets. The endpoint has a 500 ms dial and an ordinary `ClientTlsConfig` with a 4 s timeout, and the same 4 s is passed to the helper. The test asserts a `Transport` error whose cause chain says "TLS handshake timeout", and elapsed ≥ 4 s, with a 60 s watchdog. Round 2 only moved its peer and assertions into shared helpers.
- **T5 (new)** `an_internal_rpc_connect_fails_at_its_transports_handshake_bound` (real time, about 4 s). This is the behavioural test through the new entry point.
  - It builds `ClientTransport` with `from_environment_with_tls_handshake_timeout(…, 4 s)`. This is a crate-private constructor that `from_environment` itself delegates to with `CLIENT_TLS_HANDSHAKE_TIMEOUT`; it is not a test-only global.
  - It then calls `connect_endpoint` and `ConnectEndpoint::connect_with_retry` with `GrpcRetryPolicy::none()` against the silent loopback peer.
  - It asserts the TLS-handshake-timeout cause, not `AttemptTimeout`, and elapsed ≥ 4 s.
  - The 4 s bound outlasts the 3.5 s DNS + dial shares, so the test fails unless the applied bound and the passed bound agree.
- **T4 removed.** It compared the cap with its own definition and could not fail.

Mutation matrix (each mutation applied alone, then reverted; restored tree passes):

| Mutation | T1 | T2 | T3 | T5 |
|---|---|---|---|---|
| N1: `ClientTransport`'s TLS config without `.timeout(...)` | PASS | PASS | PASS | **FAIL** (7.5 s, `AttemptTimeout`) |
| N2: TLS config applies a different bound than it records (`CLIENT_TLS_HANDSHAKE_TIMEOUT` instead of `handshake_timeout`) | PASS | PASS | PASS | **FAIL** (7.5 s) |
| N3: `connect_endpoint` reports no bound for HTTPS | PASS | PASS | PASS | **FAIL** (3.5 s, `AttemptTimeout`) |
| N4: `ConnectEndpoint::connect_with_retry` passes `None` | PASS | PASS | PASS | **FAIL** (3.5 s) |
| M1: the cap ignores the TLS bound | PASS | PASS | **FAIL** (3.5 s) | **FAIL** (3.5 s) |
| M4: no per-attempt timeout | **FAIL** | **FAIL** | PASS | PASS |

The round-2 bug class ("an internal-RPC caller passes `None`") can no longer be written: those callers don't pass a bound.

Stability over 300 runs each:
- T1 and T2: 300/300 (1.2 s total).
- T3 and T5 together: 300/300 (1210.4 s total; each iteration 4.03–4.09 s).

## Validation (round 2, `545a7fe`)

### Step 1

- `cargo fmt --all -- --check` and `scripts/check-rust-error-policy.sh`: clean.
- Workspace clippy with CI's flags (`--workspace --locked --all-targets --all-features -D warnings`) plus CI's three feature-isolation runs: clean, 0 warnings.
- nextest (`SPACE_CI_REQUIRE_TEST_DEPS=1`, the repo's Postgres and Redis setup scripts) for `pplx-space-common-grpc`, its 70 workspace dependents and `pplx-space-db-test-fixtures`: **7687 run, 7653 passed, 34 failed, 31 skipped.**
  - The 34 are the same breakdown as rounds 0 and 1: 32 spaced-guest-server and 1 spaced-host-cli (process start fails with `Permission denied` / `AssumeIdentity EPERM` for this container user), plus 1 net-gateway `tls_interception::interception_exemptions_relay_original_tls_only_after_admission`.
  - Round 0 showed base `bea0d3f` fails the same set → **PRE-EXISTING / ENVIRONMENTAL**.
  - **0 CHANGE-INDUCED.** I did not run the root-only `privileged` group as root.

### Step 2

- **Box event:** the devbox container restarted at about 02:45 UTC (`/dev/kvm` recreated). `/tmp` and system packages outside `$HOME` were wiped, including `lsof` and the AWS CLI install that `~/.local/bin/aws` links to.
  - The first e2e attempt stopped at its prerequisite check.
  - I re-ran `install-prereqs.sh` and the device-mapper step exactly as CI does, then re-ran the bring-up.
  - The stress run and nextest above all started after the restart.
- Docker root had 24.5 GiB free before the bring-up (> 20 GiB), so I removed no images.
- Bring-up: `smoke-kind-e2e.sh` with `DATA_PLANE_URL=http://127.0.0.1:8090`, `SPACE_KIND_OBSERVABILITY=off`, `SPACE_KIND_TELEMETRY_MODE=datadog`, on a fresh cluster. The synced `rust-src` contains `ConnectEndpoint` and no `client_tls_handshake_timeout`. E2E round trip PASS in 6.6 min.
- Typed suite on the same cluster: `SKIP_BOOTSTRAP=1`, CI's shard env (admin token file, cold-receipts dir, data-plane health gate), `smoke-kind-typed.sh all` with no shard argument.
  Typed suite results (47.6 min):
  - ECW surface: **76 passed, 0 failed**.
  - Native surface: **23 passed, 0 failed, 0 inconclusive**. No rerun was needed.
- Bound-firing check:
  - None of the 51 `space` pods' logs, and nothing in the aggregated Tilt log, contains "did not finish within", "TLS handshake timeout" or `HandshakeTimeout`.
  - The Tilt log has 4 "exhausted TCP gRPC connect retry budget" warnings from credmgr clients to `https://space-credmgr-authority:9810/`. All are at 03:15:07–09, during bring-up and before the e2e smoke passed at 03:16:31. Each shows 12–13 fast attempts with "transport error" over 9.3–10.6 s.
  - In that window the log shows only startup failures: 168 `Connection refused`, 76 `dns error` and 38 "failed to lookup address information". This is the same pattern as round 1.
  - **Classification: ENVIRONMENTAL cold-start ordering.** Neither new bound fired, and the services recovered without intervention.
- Cleanup: `teardown.sh` stopped Tilt and the port-forwards and deleted cluster `space-local`. No clusters, Tilt processes or containers remain, and no images were pruned.

## Earlier rounds (for reference)

- **Round 0 (`8fe3a6f`):** nextest 7652/7686 (same 34). Kind: e2e PASS, ECW 76/76, native 22 + 1 inconclusive that passed on rerun (environmental).
- **Round 1 (`d78eec5`):** nextest 7653/7687 (same 34). Kind: e2e PASS, ECW 76/76, native 23/23. Four credmgr budget exhaustions during bring-up were start-up ordering ("dns error" before the authority Service existed).

## Follow-ups (out of scope)

- Replay-safe classification of connect-phase timeouts. `is_client_connect_failure` recognizes only `ConnectionRefused` and `NotFound`, and tonic's `TlsError` is crate-private, so a TLS timeout needs a typed SPACE error first.
- A retry-metric reason label for capped attempts. They are counted under the existing `connect_error` today.
- The credmgr authority listener has no server-side handshake deadline (`credential/mgr/server/src/run.rs` ~1312/1348).
- The credmgr server's key-broker client (`run.rs` ~841, `connect_lazy`) sets no client handshake bound.
- TLS inside a redials dial is not retried within that dial; doing so would mean moving TLS into `RetryingTcpConnector`.
- Name resolution on later reconnects of a returned channel is still unbounded.
- `GrpcClient`'s 1 s `NodeApiClient` connect-attempt cap undercuts a queued internal-TLS handshake.

## Decisions recorded

1. Internal RPC owns both halves. `ClientTls` builds the config's timeout from the bound it stores, and `ConnectEndpoint` carries that bound to the helpers. I chose this over a scheme-derived accessor, which could drift and which callers could ignore.
2. `ConnectEndpoint` rather than methods that take an unprepared endpoint. Applying the transport fails with `TransportError` and connecting fails with `TcpConnectError`, so the two stages keep their existing error handling in the callers.
3. The test seam is a crate-private constructor that `from_environment` delegates to, not a global.
4. credmgr returns its config and bound from one function, because the change was small.
5. `WorkerChannels`' outer wait uses `internal_transport::retrying_redial_bound` (`CLIENT_TLS_HANDSHAKE_TIMEOUT`) regardless of scheme; a plaintext dial ends sooner on its own.
6. The typed Kind suite runs unsharded on one cluster.
7. No `Co-Authored-By` trailer, per the push rule.

## Results

| Check | Result |
|---|---|
| fmt + error-policy | PASS |
| Workspace clippy (CI flags) + 3 isolation runs | PASS (0 warnings) |
| nextest common-grpc + 70 dependents | 7653/7687 pass; 34 PRE-EXISTING/ENVIRONMENTAL (same set as base); 0 CHANGE-INDUCED |
| Mutations N1–N4, M1, M4 | every new or changed test fails under at least one; T5 catches every way the applied and passed bounds can diverge |
| 300× stress | T1, T2 300/300; T3 + T5 300/300 |
| Kind bring-up + e2e smoke | PASS (after re-provisioning prerequisites lost in a container restart) |
| Typed Kind, ECW surface | 76/76 PASS |
| Typed Kind, native surface | 23/23 PASS |
| Cap or TLS timeout fired in Kind | never (4 fast startup-ordering exhaustions during bring-up, ENVIRONMENTAL) |
| Cleanup | done; no images pruned |

**Verdict: ready.** The round-2 MAJOR is fixed by construction. The internal transport builds each TLS config's timeout from the bound it stores, and `ConnectEndpoint` passes that same bound to the connect helpers. Every internal-RPC caller of the helpers now goes through it, and none states a bound. A behavioural test through `ConnectEndpoint` fails if the applied bound is removed, changed, misreported or not passed on. No validation failure was caused by the change.
