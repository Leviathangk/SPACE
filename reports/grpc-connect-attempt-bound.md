# Bound every whole TCP gRPC connect attempt in the shared connect helper

- Base: `main` at `bea0d3f2bc8d82ab11ccc1e7017b899bd3b3d1e2`
- Change: `ac/grpc-connect-attempt-bound`:
  - `8fe3a6f` — round 0: per-attempt cap, TLS bounds.
  - `d78eec5` — review round 1: caller-sized caps, a 15 s internal-transport handshake bound derived from the server's deadline.
- Box: Linux x86_64, 16 vCPU, KVM; Rust 1.98.0 (pinned), protoc 35.0, nextest 0.9.133

## Problem (verified against the vendored sources)

`retry_tcp_connect` checks `max_elapsed` only between attempts. Nothing bounded a single attempt.

In tonic 0.14.6, `Endpoint::connect()` builds an `HttpConnector` with `set_connect_timeout`. In hyper-util 0.1.20:
- `call_async` resolves the name first, and nothing bounds that lookup.
- The timeout then applies only to the TCP connects.
- TLS runs after the inner connector (tonic `channel/service/connector.rs`). Nothing bounds it unless the `ClientTlsConfig` sets a timeout.
- The HTTP/2 handshake follows. It only writes the client preface to a fresh socket.

The build-manager client sets no `connect_timeout`, so even its TCP connect had no bound.

## Design (after review round 1)

### Per-attempt cap

`retry_tcp_connect` runs each attempt under `tokio::time::timeout(attempt_cap, …)`, as `retry_uds_connect` already did. An attempt that hits the cap:
- fails as `TcpConnectError::AttemptTimeout { service_name, target, cap }`;
- goes through the same span status, `client_retry` metric, retry log and exhaustion warning as any failed attempt;
- is redialed while the budget remains.

Budgets and backoff are unchanged.

**Cap formula:**

> cap = `NAME_RESOLUTION_ALLOWANCE` (3 s) + the endpoint's dial timeout (`get_connect_timeout()`, 3 s when unset) + the caller's TLS handshake bound (0 for plaintext)

Consequences:
- An attempt whose lookup and dial stay within their shares fits the cap. So a handshake that reaches its own TLS bound fails with that cause (tonic "TLS handshake timeout") before `AttemptTimeout` fires.
- The worst-case total of a call is `max_elapsed` + one cap. A new attempt starts only while `elapsed < max_elapsed`, and it runs at most one cap.

**How the TLS bound reaches the helper.** tonic gives no way to read an endpoint's TLS config. So `connect_tcp_channel_with_retry` and `connect_tcp_channel_with_retrying_redials` now take `tls_handshake_timeout: Option<Duration>`, with `None` meaning plaintext.
- I chose an explicit parameter because it is the least invasive typed option: every caller must state its bound, and the compiler enforces that.
- Internal-transport callers forward `internal_transport::client_tls_handshake_timeout(&endpoint)`. It returns the bound `ClientTransport::endpoint` sets for HTTPS, and `None` for plaintext.
- I rejected a wrapper type carrying the bound. It would change `ClientTransport::endpoint`'s return type for about ten callers that never use the helpers.

### TLS handshake bounds

| Client | Bound | Why |
|---|---|---|
| Internal transport (`ClientTransport`: node-api, build-mgr, `WorkerChannels`, data plane, node-ctl, transfer, activation, cluster discovery) | `CLIENT_TLS_HANDSHAKE_TIMEOUT` = `SERVER_TLS_HANDSHAKE_TIMEOUT` (10 s, now a named constant used by `server_builder_from_environment`) + 5 s = **15 s**, derived in code | The listener takes one of 64 handshake slots before it polls accept (`internal_transport/incoming.rs`). While every slot is busy, a connected socket waits in the kernel accept queue. That wait counts against the client's timer, which starts at TCP connect, but not against the server's deadline, which starts at accept. The extra 5 s absorb a short queue. No client bound can guarantee a handshake while a listener stays saturated. |
| credmgr `Tls` (`credential/mgr/client/src/endpoint.rs`) | credmgr-specific `TLS_HANDSHAKE_TIMEOUT` = **3 s**; #309's 3 s TCP `connect_timeout` is kept | The credmgr listener hands each accepted socket straight to tonic TLS. There are no admission slots, so there is no queue wait to cover. |

`ClientTlsConfig::timeout` exists in tonic 0.14.6. It wraps `RustlsConnector::connect` in `tokio::time::timeout`, covers only the handshake, and fails with `TlsError::HandshakeTimeout`. It applies to every connect the channel makes, reconnects included. `docs/internal-rpc-tls.md` now states the 15 s client bound and why it is derived from the server's deadline.

### Resulting numbers (10 s default budget, `DEFAULT_TCP_GRPC_CONNECT_RETRY_POLICY`)

| Caller class | Cap (DNS 3 + dial 3 + TLS) | Worst-case total |
|---|---|---|
| Internal-TLS callers (api node-api and builder, build-mgr) | 3 + 3 + 15 = **21 s** | 10 + 21 = **31 s** |
| credmgr TLS | 3 + 3 + 3 = **9 s** | **19 s** |
| credmgr plaintext (also internal transport in insecure dev mode) | 3 + 3 = **6 s** | **16 s** |

Both helpers' doc comments state these.

### Redials helper and `WorkerChannels`

- The per-attempt cap inside `RetryingTcpConnector` covers DNS + TCP only. It is now `NAME_RESOLUTION_ALLOWANCE` + `attempt_timeout` = 6 s, with no TLS constant.
- The new public `retrying_redial_bound(policy, attempt_timeout, tls)` = `max_elapsed` + attempt cap + TLS bound. It is now:
  - the redials helper's endpoint `connect_timeout`, which tonic applies to the whole connector, TLS included, for every dial and redial;
  - `WorkerChannels::dial_budget`. It uses the internal-TLS bound as an upper bound regardless of scheme; a plaintext dial ends sooner on its own.
- Whole-dial bound: 10 + 6 + 15 = **31 s** for internal TLS, 16 s for plaintext. Before this fix it was 13 s, which would have cut the 15 s handshake. Neither outer bound truncates it any more.

### Error compatibility

- `connect_tcp_channel_with_retry` returns `TcpConnectError`: a transparent `Transport(tonic::transport::Error)` plus `AttemptTimeout`. tonic has no public constructor for a transport error.
- api callers only format the error, so their messages are unchanged.
- The credmgr and build-mgr `ConnectError` types map `Transport` to their existing `Transport` variant, and `AttemptTimeout` to a new `TcpConnect` variant.
- build-mgr `object_store::is_retriable` matches credmgr errors exhaustively; it now classes `TcpConnect` as retriable.

## Caller audit

| Caller | Path | Dial | TLS bound | Dials to | Effect of the new bounds |
|---|---|---|---|---|---|
| api `connect_worker`, `connect_worker_internal`, `connect_builder` (`substrate/mod.rs` ~3915–3970) | `connect_tcp_channel_with_retry` | 3 s | 15 s (internal TLS) | Kubernetes: pod IP literal; direct: node address from the DB | cap 21 s, worst 31 s; reconnects: TLS 15 s |
| api `builder_node_client_at` (`kubernetes/pod_ops/worker_clients.rs` ~150–175) | `connect_tcp_channel_with_retry` | 3 s | 15 s | DNS name `space-builder-node-api:9800` | cap 21 s, worst 31 s |
| build-mgr client `connect_channel` | `connect_tcp_channel_with_retry` | none (3 s assumed) | 15 s | build-manager DNS name | cap 21 s, worst 31 s (TCP had no bound before) |
| credmgr `Tcp` | `connect_tcp_channel_with_retry` | 3 s | none | Service DNS name | cap 6 s, worst 16 s |
| credmgr `Tls` | `connect_tcp_channel_with_retry` | 3 s | 3 s | Service DNS name | cap 9 s, worst 19 s; every handshake 3 s |
| api `WorkerChannels::dial` (`worker_channels.rs`) | `connect_tcp_channel_with_retrying_redials` | 3 s | 15 s | direct node address | attempt 6 s; whole dial or redial and shared-dial wait 31 s |
| api `pod_worker_channel` (`kubernetes/pod_ops/worker_clients.rs` ~29–47), used by wait-ready and wake (500 ms dial, `START_WAIT_CONNECT_TIMEOUT`) and teardown (3 s); node eligibility only resolves the pod endpoint these dial | direct `Endpoint::connect()` (no helper) | 500 ms or 3 s | 15 s | pod IP literal | No attempt cap. A handshake that never completes is now cut at 15 s; before, nothing bounded it except the callers' outer waits. TCP and DNS are unchanged (IP literal, no lookup). |
| api admin `node_maintenance` (`routes/admin/node_maintenance.rs` ~535) | direct `client_endpoint` + `connect()` | 5 s | 15 s | operator-supplied node endpoint | No attempt cap. Handshake now bounded at 15 s; DNS still unbounded. |
| `NodeApiClient` (`node/api/client/src/lib.rs` ~288–304) | `GrpcClient` connect closure via `ClientTransport` | 3 s | 15 s | node-api endpoint | `GrpcClient` already bounds each whole connect attempt at 1 s (`DEFAULT_CONNECT_ATTEMPT_TIMEOUT`), so the 15 s bound only matters on the cached channel's later reconnects. Note that this pre-existing 1 s attempt bound undercuts a queued handshake in the same way the review describes. I left it unchanged; see follow-ups. |

The only production `connect_tcp_channel_with_retrying_redials` call site is `worker_channels.rs`; `direct.rs` builds the `WorkerChannels` that uses it.

## Tests (`crates/libs/common/grpc/src/connect/tests.rs`)

1. `a_stalled_tcp_attempt_is_abandoned_at_its_cap_and_redialed` (paused time, `retry_tcp_connect` closure). Unchanged: attempt 1 never resolves, attempt 2 connects, and elapsed time is exactly the cap plus the first backoff.
2. `stalled_tcp_attempts_end_within_the_budget_and_one_attempt_cap` (paused time). Unchanged: the result is `AttemptTimeout` with the cap, more than one attempt ran, and `max_elapsed < elapsed ≤ max_elapsed + cap`.
3. `a_tls_peer_that_never_answers_fails_the_connect_at_its_handshake_bound`, changed. It runs in real time at about 4 s, with no sleeps.
   - A loopback listener accepts and holds every socket.
   - The endpoint uses an ordinary `ClientTlsConfig` with `.timeout(4 s)` and a 500 ms dial, and the same 4 s is passed as the helper's `tls_handshake_timeout`.
   - The test asserts a `Transport` error whose source chain contains "TLS handshake timeout", and elapsed ≥ the bound. The upper bound is gone; a 60 s watchdog remains.
   - The 4 s bound outlasts the 3.5 s DNS + dial shares, so the test passes only if the cap counts the TLS bound.
4. `an_internal_tls_attempt_cap_leaves_room_for_its_handshake_bound`, new.
   - It builds an `https` and an `http` endpoint through `ClientTransport::from_environment` (rcgen CA, insecure-dev opt-in for the plaintext case).
   - It asserts that `client_tls_handshake_timeout` reports a bound only for HTTPS.
   - It asserts that the endpoint's attempt cap is strictly greater than dial + handshake bound, i.e. it leaves room for name resolution.

No test asserts a configured value.

Mutation matrix (each mutation applied alone, then reverted; restored tree passes):

| Mutation | T1 | T2 | T3 | T4 |
|---|---|---|---|---|
| M1: endpoint cap ignores the caller's TLS bound | PASS | PASS | **FAIL** (3.5 s: `AttemptTimeout` instead of the TLS cause) | **FAIL** |
| M2: drop the name-resolution allowance | PASS | PASS | PASS | **FAIL** |
| M3: `client_tls_handshake_timeout` reports no bound | PASS | PASS | PASS | **FAIL** |
| M4: drop the per-attempt `tokio::time::timeout` | **FAIL** | **FAIL** | PASS | PASS |

Stability over 300 runs each:
- T1, T2 and T4: 300/300 (8.0 s total).
- T3: 300/300 (1211.8 s; each run took 4.03–4.56 s).

**Coverage gap:** no test proves at runtime that `ClientTransport` applies the 15 s timeout. tonic's TLS config cannot be inspected, and a behavioural test would take 15 s of real time. T4 covers the reporting side, and a source read covers the setting itself.

## Validation

### Step 1 (round 2)

- `cargo fmt --all -- --check`, `scripts/check-rust-error-policy.sh`: clean.
- Workspace clippy with CI's flags (`--workspace --locked --all-targets --all-features -D warnings`) plus CI's observability-otel, common-svc no-default and feature-flags no-default runs: clean, 0 warnings.
- nextest (`SPACE_CI_REQUIRE_TEST_DEPS=1`, repo Postgres and Redis setup scripts) for `pplx-space-common-grpc` + its 70 workspace dependents + `pplx-space-db-test-fixtures` (which provides the setup binary): **7687 run, 7653 passed, 34 failed, 31 skipped.**
  - The failing set is identical to round 1's.
  - 33 in spaced-guest-server and spaced-host-cli: process start hits `Permission denied` or `AssumeIdentity EPERM`. Round 1 showed base `bea0d3f` fails the same 33 on this box → **PRE-EXISTING / ENVIRONMENTAL**.
  - 1 in net-gateway `tls_interception::interception_exemptions_relay_original_tls_only_after_admission`: also fails on base → **PRE-EXISTING**.
  - **0 CHANGE-INDUCED.** I did not run the root-only `privileged` group as root.

### Step 2 (round 2)

- Docker root had 28.8 GiB free before the bring-up (> 20 GiB), so no image cleanup was needed.
- Bring-up: `smoke-kind-e2e.sh` with `DATA_PLANE_URL=http://127.0.0.1:8090`, `SPACE_KIND_OBSERVABILITY=off`, `SPACE_KIND_TELEMETRY_MODE=datadog`. It started a fresh cluster and built from the `d78eec5` sources (the synced `rust-src` contains `SERVER_TLS_HANDSHAKE_TIMEOUT`). E2E round trip PASS in 8 min.
- Typed suite on the same cluster: `SKIP_BOOTSTRAP=1`, CI's shard env (admin token file, cold-receipts dir, data-plane health gate), `smoke-kind-typed.sh all` with no shard argument (full inventory on one cluster).
  Typed suite results (47.5 min):
  - ECW surface: **76 passed, 0 failed**.
  - Native surface: **23 passed, 0 failed, 0 inconclusive**. No rerun was needed.
- Bound-firing check:
  - None of the 51 `space` pods' logs contain "did not finish within", "TLS handshake timeout" or an exhausted connect budget.
  - The aggregated Tilt log has 4 "exhausted TCP gRPC connect retry budget" warnings from credmgr clients to `https://space-credmgr-authority:9810/`, all at 22:25:49 during bring-up and before the e2e smoke passed at 22:27.
  - Each shows 13 attempts in about 9.4 s with a plain "transport error". These were fast failures well inside the 9 s cap. The neighbouring lines report "dns error" because the authority Service was not resolvable yet, and the authority pods were scheduled afterwards.
  - **Classification: ENVIRONMENTAL cold-start ordering.** Neither new bound fired, and fast failures retry exactly as before the change. The services recovered without intervention.
- Cleanup: `teardown.sh` stopped Tilt and the port-forwards and deleted cluster `space-local`. No clusters, Tilt processes or containers remain, and no images were pruned.

### Round 0 results (for reference, `8fe3a6f`)

- nextest: 7652/7686 passed, with the same 34 pre-existing failures.
- Kind: e2e PASS. Typed ECW 76/76. Native 22 passed + 1 inconclusive that passed on rerun (environmental).
- No cap or TLS-timeout firings.

## Follow-ups (out of scope)

- **Replay-safe classification of connect-phase timeouts.** `is_client_connect_failure` recognizes only `ConnectionRefused` and `NotFound`. tonic's `TlsError` is crate-private, so a TLS handshake timeout needs a typed SPACE error before it can be classified as never-sent.
- **A retry-metric reason label for capped attempts.** They are counted under the existing `connect_error` reason today.
- **The credmgr authority listener has no server-side handshake deadline.** It uses plain tonic TLS with no admission gate (`credential/mgr/server/src/run.rs` ~1312/1348).
- **The credmgr server's key-broker client** (`run.rs` ~841, `connect_lazy`) sets no client handshake bound.
- **TLS inside a redials dial.** It runs after the retried attempt, so a stuck handshake fails the dial instead of being redialed. Retrying it would mean moving TLS into `RetryingTcpConnector`.
- **Reconnects of `connect_tcp_channel_with_retry` channels** still resolve DNS with no bound.
- **`GrpcClient`'s 1 s `NodeApiClient` connect-attempt bound** undercuts a queued internal-TLS handshake, as described above.

## Decisions recorded

1. The cap is the DNS allowance + the endpoint's dial timeout + the caller's TLS bound, computed per call. No policy field: the bound depends on each caller's own dial and TLS, and a new field would have broken about 20 struct literals.
2. The TLS bound is an explicit `Option<Duration>` parameter, with `client_tls_handshake_timeout` for internal-transport callers (reasons above).
3. The internal client bound is the server deadline + 5 s, derived in code. credmgr keeps its own 3 s.
4. `WorkerChannels::dial_budget` uses the internal-TLS bound regardless of scheme, as a safe outer bound.
5. Test 3 uses a natural `ClientTlsConfig::timeout` bound of 4 s, longer than its 3.5 s DNS + dial shares, so it also proves that the cap counts the TLS bound.
6. Typed Kind suite run unsharded on one cluster.
7. No `Co-Authored-By` trailer, per the push rule.

## Results

| Check | Result |
|---|---|
| fmt + error-policy | PASS |
| Workspace clippy (CI flags) + 3 isolation runs | PASS (0 warnings) |
| nextest common-grpc + 70 dependents | 7653/7687 pass; 34 PRE-EXISTING/ENVIRONMENTAL (identical set, fail on base); 0 CHANGE-INDUCED |
| Mutations (M1–M4) | every new or changed test fails under at least one |
| 300× stress (T1–T4) | 300/300 each |
| Kind bring-up + e2e smoke | PASS |
| Typed Kind, ECW surface | 76/76 PASS |
| Typed Kind, native surface | 23/23 PASS |
| Cap or TLS timeout fired in Kind | never (4 fast startup-ordering budget exhaustions during bring-up, ENVIRONMENTAL) |
| Cleanup | done; no images pruned |

**Verdict: ready.** The major finding is fixed. The internal-transport client handshake bound is now the listener's 10 s deadline + 5 s = 15 s, derived in code. Each caller's attempt cap includes its own TLS bound plus a separate name-resolution allowance, so a handshake timeout surfaces with its own cause. The resulting caps and worst cases are 21 s / 31 s (internal TLS), 9 s / 19 s (credmgr TLS) and 6 s / 16 s (plaintext). Neither redial outer bound cuts the 15 s handshake any more. No validation failure was caused by the change.
