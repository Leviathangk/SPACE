# TCP connect-phase failures replay-safe via a typed SPACE marker

Feature branch `ac/grpc-replay-safe-connect-phase`:
- `8234955` adds the marker, the connector, the classifier change and the
  caller routing.
- `de59314` is the review round 1 fixes, one commit on top.

Base: `main` `64afd0b`. No pull request was opened.

## Design (as of `de59314`)

**Marker.** `pplx_space_common_grpc::ConnectPhaseError` is a public
`thiserror` type that wraps a boxed error.
- Its `Display` is the source's message and `source()` is the original error,
  the same convention as tonic's own `ConnectError`. So status messages are
  unchanged (`"tcp connect error"`), and the existing
  `ConnectionRefused`/`NotFound` check still finds its `io::Error` below the
  marker.
- Only SPACE's connectors construct it. The constructor is private.

**Connector.** `TcpConnector` (private, `connect.rs`) resolves and dials
through hyper-util's `HttpConnector` and maps every `poll_ready`/`call` error
to `ConnectPhaseError`.
- The `HttpConnector` is configured from the `Endpoint` getters the way tonic
  0.14.6's `Endpoint::http_connector()` configures it: `enforce_http(false)`,
  nodelay, keepalive, keepalive interval/retries and the dial bound.
- Two deviations, both documented on the connector:
  - The local address is not carried over. Tonic has no getter for it; the
    kernel chooses, and no SPACE endpoint sets one.
  - An endpoint without a `connect_timeout` gets the 3 s
    `DEFAULT_TCP_DIAL_TIMEOUT`, as #360 already did in
    `connect_tcp_channel_with_retry`, so every routed channel has a dial bound.
- Since `de59314` the connector bounds name resolution and the dial
  **together** at `connect_phase_bound(d) = 3 s + d`. On expiry it returns a
  `ConnectPhaseError` wrapping `ConnectPhaseTimeout` ("name resolution and TCP
  dial did not finish within …"), so the failure is marked, `Unavailable` and
  replay-safe.
- It is generic over the inner connector, because hyper-util's `Resolve` trait
  is sealed. That is how a test injects a resolver that never answers.
- The retrying-redial connector wraps the same `TcpConnector` (dial bound
  `attempt_timeout`). It marks its final error, including an attempt it
  abandoned at its cap.

**The `TimeoutConnector` trap and stall ordering.** `connect_with_connector[_lazy]`
wraps the whole connector, TLS included, in tonic's `TimeoutConnector` at the
endpoint's `connect_timeout`. `connect()` gives that value to the dial only.
With round 1's equal bounds, tonic's timer fired first on a stalled lookup:
hyper-timeout's bare `io::Error(TimedOut)` sits outside tonic's `ConnectError`,
so the RPC failed as an unmarked `Unknown`. Now each stall reports its own
cause, in this order:

1. The connect phase (lookup plus dial) fails at `3 s + d`: a marked
   `ConnectPhaseError`. A dial that reaches `d` after a quick lookup fails
   earlier, with its own marked `TimedOut`.
2. The TLS handshake fails at its own bound `T` (`ClientTlsConfig::timeout`).
   It is unmarked, by design.
3. The retry helper's `AttemptTimeout` fires `ATTEMPT_CAP_MARGIN` = 0.5 s
   after both: attempt cap = `3 + d + T + 0.5 s`.
4. Tonic's whole-connect timeout is set to `3 + d + T +
   WHOLE_CONNECT_BACKSTOP_MARGIN` (1 s). It is a backstop that should never
   fire first. If it ever does, its failure is conservatively unmarked
   (`Unknown`), as documented.

For the retrying-redial connector, `retrying_redial_bound` = `max_elapsed +
(3 + a + 0.5 s) + T + 1 s`.

**Fail closed.**
- `connect_tcp_channel`, `connect_tcp_channel_lazy`,
  `connect_tcp_channel_with_retry`, `connect_tcp_channel_with_retrying_redials`
  and `RotatingChannel::connect` all check the endpoint first.
- They return a typed `TcpEndpointError`, through `TcpChannelError::Endpoint`
  or `TcpConnectError::Endpoint`, in two cases:
  - `MissingTlsHandshakeBound`: an `https` endpoint without its handshake
    bound.
  - `UnixSocket`: a tonic Unix-socket endpoint. Tonic exposes no accessor for
    this, so it is recognised by tonic's fixed fallback URI `http://tonic`,
    which no SPACE TCP endpoint uses.
- `ClientTransport::connect_endpoint` also refuses a Unix-socket endpoint
  (`TransportError::Scheme`). `ConnectEndpoint` therefore guarantees both
  properties by construction, and its `connect`/`connect_lazy`/
  `connect_rotating`/`connect_with_retrying_redials` keep their signatures,
  calling crate-internal unchecked variants.
- `internal_transport::client_endpoint` was removed. Its last caller, a
  build-mgr test, now uses `client_connect_endpoint`.

**TLS.** Tonic layers TLS over the connector's stream, so TLS errors
(including `HandshakeTimeout`) never carry the marker and stay not
replay-safe. This is intended and covered by a test.

**Classifier.** At each level of the local chain, `is_client_connect_failure`
returns true on `ConnectPhaseError`, before the existing first-`io::Error`
check. The `Unavailable` gate and the `ConnectionRefused`/`NotFound` check are
kept. A bare `TimedOut`, or any other io kind, without the marker stays false.

Observed chains on the live Kind stack (node-ctl's lazy cluster-api channel):
`transport::Error → tonic ConnectError → ConnectPhaseError → hyper-util
ConnectError("dns error" | "tcp connect error") → io`.

## Caller table (bounds as of `de59314`)

Notation:
- `d` = the endpoint's dial bound.
- `T` = TLS handshake bound: 15 s for internal https, none for dev plaintext.
- "phase" = the connector's lookup+dial bound, 3+d.
- "backstop" = tonic's whole-connect bound, 3+d+T+1.

Before this branch, every `connect()`/`connect_lazy()` path had an unbounded
name lookup and no whole-connect bound.

| Caller | Connect path (before → after) | Dial / phase / attempt cap / backstop (https; http) |
|---|---|---|
| `connect_tcp_channel_with_retry` via `ConnectEndpoint::connect_with_retry` (api `connect_worker`/`_internal`/`_builder`, pod-ops internal client, build-mgr `connect`) | `endpoint.connect()` per attempt → SPACE connector per attempt and on every later reconnect | d (3 s default) / 6 s / 21.5 s; 6.5 s / 22 s; 7 s. Whole call with the default policy (10 s): 31.5 s; 16.5 s |
| credmgr client (Tcp, Tls) via `connect_tcp_channel_with_retry` | same | 3 s / 6 s / Tls: 9.5 s (its own 3 s TLS bound), Tcp: 6.5 s / Tls: 10 s, Tcp: 7 s. Whole call: 19.5 s; 16.5 s |
| `connect_tcp_channel_with_retrying_redials` (api `WorkerChannels`, a=3 s) | raw `HttpConnector`, unmarked → `TcpConnector` per attempt, final error marked | 3 s / 6 s / 6.5 s per attempt / `retrying_redial_bound` 32.5 s; 17.5 s |
| Data-plane backend pool (lazy) | `client_endpoint` + `connect_lazy` → `ConnectEndpoint::connect_lazy` | 3 s / 6 s / — / 22 s; 7 s |
| Ingress activation (lazy) | same change | 3 s / 6 s / — / 22 s; 7 s |
| node-ctl cluster-api client (lazy) | same change | 5 s / 8 s / — / 24 s; 9 s |
| Transfer source (one-off) | `client_endpoint` + `connect` → `ConnectEndpoint::connect` | 10 s / 13 s / — / 29 s; 14 s |
| `pod_worker_channel` (one-off) | `node_api_transport` + `connect` → `node_api_connect_endpoint` + `connect` (`node_api_transport` removed) | c / 3+c / — / 3+c+T+1 |
| Admin node-maintenance verify (one-off) | same as transfer | 5 s / 8 s / — / 24 s; 9 s |
| `NodeApiClient` connect closure (trigger fanout d=2 s, substrate/sandbox-service-ports d=3 s) | `transport.endpoint(..).connect()` → `transport.connect_endpoint(..).connect()` | d / 3+d / `GrpcClient`'s unchanged 1 s attempt cap on the eager connect / 3+d+T+1 (e.g. 21 s, 22 s; 6 s, 7 s) |
| Cluster-api discovery probe and `RotatingChannel` (first dial and rotations) | `endpoint.connect()` → `connect_endpoint(..).connect()` / `.connect_rotating(..)` | 5 s / 8 s / — / 24 s; 9 s |
| Build-mgr lazy client | `endpoint` + `connect_lazy` with no dial bound (unbounded) → `connect_endpoint` + `connect_lazy` | now 3 s default / 6 s / — / 22 s; 7 s |
| Not routed: tonic balanced channel (grpc-lb cluster-api discovery) | tonic-internal `http_connector()`; `Channel::balance_channel` has no connector hook | unchanged, unmarked |
| Not routed: credmgr intermediate key-broker channel (not internal transport), observability trace-parity fixture (test), all UDS/vsock channels | — | unchanged |

## What each `is_client_connect_failure` caller now replays

Newly classified as never sent: any SPACE-connector failure. That means a
dial that reached its bound, EHOSTUNREACH/ENETUNREACH, a DNS failure or a
lookup stalled past the phase bound, a bad URI, and, on kept worker channels,
a redial whose retries ran out (including abandoned attempts). Retry budgets,
backoff and RPC selection are unchanged.

| Caller | Newly replayed or reclassified | Budget that bounds it |
|---|---|---|
| api `RuntimeClient::call` (`runtime_rpc.rs`), on node-api TCP channels | One replay on a freshly probed client after such a failure; before, only a refused dial earned it | Unchanged: at most one replay, all inside `timeout_at(deadline)`, each attempt's grpc-timeout set to the remaining deadline |
| api access-idle update (`classify_access_idle_send_failure`, `updates.rs`) | The update goes through `RuntimeClient::call`, so such a failure is **replayed once** there first (row above). Only if that replay also fails this way is it classified `Untransmitted`: the paced 503 `unconfirmed_access_idle_update` (retry-after 1 s) instead of `Ambiguous`, which did one State observation | Unchanged: the same single replay and deadline |
| node-api `restart_handoff` | Nothing: forwards to sandbox-manager over UDS, which never carries the marker | Unchanged |
| node-mgr `runtime_supervisor_client` | Nothing: node-local Unix socket | Unchanged |

(Correction from the earlier report: "never resends" was wrong for the
access-idle path. `RuntimeClient::call` replays it once first. The credmgr
row used to quote the internal-transport 21 s; credmgr's own bounds are 3 s
dial and 3 s TLS.)

## Tests

Grpc tests are in `crates/libs/common/grpc/src/connect/tests.rs` unless noted.
The `Blackhole` fixture (a `listen(2)` listener plus three queued
connections) now lives in `pplx_space_common_grpc::test_support`, built only
for tests and the new `test-support` feature, so the api can reuse it.

Round 1 (`8234955`):
1. `a_lazy_channels_timed_out_dial_is_a_client_connect_failure`
2. `an_unmarked_timed_out_dial_is_not_a_client_connect_failure`
3. `a_refused_dial_is_a_client_connect_failure_through_either_connector`
4. `a_kept_channels_redial_that_never_connects_is_a_client_connect_failure`
5. `an_rpc_whose_connection_dies_after_it_was_sent_is_not_a_client_connect_failure`
   (close / reset). Its peer helper is refactored into `request_reading_peer`
   with Close/Reset/Hold modes.
6. `a_peers_unavailable_that_reads_like_a_connect_failure_is_not_one`
7. `a_lazy_internal_rpc_channels_tls_handshake_timeout_is_not_a_client_connect_failure`
   (`internal-tls`)

Round 2 (`de59314`):
8. `a_stalled_name_resolution_is_a_marked_client_connect_failure`: paused
   clock, real `HttpConnector` over an injected resolver that never answers.
   The RPC fails as a marked `UNAVAILABLE` exactly at `connect_phase_bound(d)`,
   and the classifier says true.
9. `a_retried_connects_stalled_lookup_fails_with_its_own_cause`: the same
   stall inside `retry_tcp_connect` fails as `TcpConnectError::Transport`
   (marked), not `AttemptTimeout`.
10. `a_lazy_channel_without_a_dial_bound_is_given_the_default_one`: paused
    clock, blackhole, endpoint without `connect_timeout`. It fails with the
    dial's marked `TimedOut` and doesn't hang.
11. `a_tcp_connect_refuses_endpoints_it_would_dial_wrongly`: `https` without
    a TLS bound, and a `unix:` endpoint, are refused with the right
    `TcpEndpointError` by `connect_tcp_channel`, `_lazy`, `_with_retry`,
    `_with_retrying_redials` and `RotatingChannel::connect`.
12. `internal_transport::tests::a_unix_socket_endpoint_is_not_an_internal_rpc_tcp_endpoint`:
    `ClientTransport::connect_endpoint` refuses a Unix-socket endpoint even
    with dev plaintext allowed.
13. `a_sent_rpc_whose_connection_goes_silent_is_not_a_client_connect_failure`:
    the peer reads the request, then holds the connection and answers nothing.
    The client's HTTP/2 keep-alive (200 ms / 200 ms) gives up, and tonic
    reports a local `UNAVAILABLE`, which the classifier must reject.
14. `tests/retry.rs::a_broken_connection_under_a_local_unavailable_is_not_a_client_connect_failure`:
    a local `UNAVAILABLE` whose source is `ConnectionReset`, `ConnectionAborted`,
    `BrokenPipe`, `TimedOut` or `UnexpectedEof` is false.
15. api `controllers::sandbox::updates::transport_tests::a_blackholed_worker_redial_is_untransmitted_and_replayed_once`:
    a production `connect_worker` channel to a live front that stops, after
    which its address is blackholed.
    - The redial times out (asserted `TimedOut`).
    - `classify_access_idle_send_failure` says `Untransmitted`.
    - `RuntimeClient::call` makes exactly 2 attempts (1 + one replay on the
      probe's fresh client) and succeeds against the sim worker.
    - The access-deadline update then applies.

No test pins a configuration value. Test 8 asserts the elapsed time against
`connect_phase_bound(d)`, computed by the code under test, to show which
bound fired.

### Mutation results

Each mutation was applied alone, then reverted. Round 1 ran the selected
tests; round 2 ran the whole grpc crate plus the api transport and
runtime-RPC tests.

| Round | Mutation | Failing tests |
|---|---|---|
| 1 | M1 connector returns unmarked errors | 1 |
| 1 | M2 accept raw `TimedOut` | 2 |
| 1 | M3 classifier ignores the marker | 1, 4 |
| 1 | M4 whole connect bounded at the dial bound (trap) | 7 + both existing handshake-bound tests |
| 1 | M5 retrying connector unmarked | 4 |
| 1 | M6 any local `io::Error` kind | 2 |
| 1 | M7 message-text fallback | 6 + existing local-chain test |
| 1 | M8 any tonic `ConnectError` (TLS included) | 7, 2 |
| 1 | M9 any local chain | 5, 2, 7 |
| 1 | M10 no code gate + any io kind | 5, 2 |
| 2 | R1 remove the connector's inner connect-phase bound | 8, 9 |
| 2 | R2 no default dial bound for endpoints without one | 10, 4 |
| 2 | R3 no fail-closed endpoint check | 11 |
| 2 | R4 no Unix-socket detection | 11, 12 |
| 2 | R5 add `ConnectionReset` to the io kinds | 14 |
| 2 | R6 any local io error counts | 14, 2 |
| 2 | R7 any local `UNAVAILABLE` counts | 13, 14, 7, 2 |
| 2 | R8 `TcpConnector` drops the marker | **15 (api)**, 1, 8, 9, 10 |
| 2 | R9 whole-connect bound cut to the dial bound (trap) | 7, 8, 9 + both existing handshake-bound tests |

R5 and R6 are caught by test 14, not by test 13. A real keep-alive failure's
chain is `hyper::Error` → hyper's own timeout type and contains no
`io::Error`, so no io-kind mutation can affect it. Test 13 catches the
mutation that would actually misclassify it, R7.

### Stress (300 runs each, on the final code `de59314`)

- **grpc:** 17 tests, the 15 new or changed plus the two existing
  handshake-bound tests whose bounds moved. 300/300 iterations, 5100/5100
  runs, 0 failures (1209 s).
- **api:** test 15. 300/300 iterations, 0 failures (1060 s).
- **Round 1, on `8234955`:** 300/300 iterations, 2400/2400 runs.
- **Cleanup:** the api stress left ~200 Postgres/Redis fixture containers.
  Their setup scripts' own cleanup helpers removed them once nextest exited;
  I waited until 0 remained.

## Validation (round 2, `de59314`)

**Step 1.**
- fmt, `check-rust-error-policy.sh`, every CI clippy invocation
  (`-D warnings`) and grpc no-features clippy: PASS.
- `RUSTDOCFLAGS=-D warnings cargo doc -p pplx-space-common-grpc`: PASS after
  removing a private intra-doc link.
- nextest `pplx-space-common-grpc`: 92/92 with all features, 92/92 with
  `internal-tls`, 84/84 with no features.
  - The first all-features run after the rebuild failed `deadline_clamps_retries`
    and `caller_classified_cancelled_respects_deadline` in the unchanged
    `tests/retry.rs`. Both use 1 ms real-clock deadlines; they passed in three
    immediate reruns and in every later run. Classified PRE-EXISTING
    (timing-flaky).
- nextest grpc + 70 dependents (+ db-test-fixtures), `--all-features
  --retries 1 -E 'not group(privileged)'`: 7827 run, 7795 passed, 32 failed,
  no flaky retries.
  - The 32 failures are exactly the set that fails on base `64afd0b`
    (spaced process-spawn tests, `PermissionDenied (os error 13)`):
    ENVIRONMENTAL / PRE-EXISTING.
- Builds used a host target dir with debuginfo off. That dir is only for host
  tests; the Kind images are built separately (see step 2).

**Step 2 (Kind, `de59314`).**
- **Disk:**
  - Docker root had 16 GB free (< 20 GiB), so I listed and removed 33 older
    `space-*:tilt-*` tags, keeping the newest per repo, to reach 18 GB. No
    chef-base/rust-build image was superseded.
  - `/home/dev` had 64 GB free. No other worktree or file was touched.
- **Build:** `build-rust-artifacts.sh` incremental release build, the same
  shape as CI. It synced 12 changed sources and compiled for 177 s. Every
  SPACE Rust image carries revision `de593143bd27`.
- **Baseline:** `smoke-kind-e2e.sh` with the specified env passed (exit 0),
  20:55–21:02 UTC.
- **Typed suite:** `SKIP_BOOTSTRAP=1 smoke-kind-typed.sh all`, unsharded,
  with the CI shard env, 21:02–21:51 UTC.
  - Native surface: 23/23.
  - ECW surface: 76 passed, 1 INFRA_ERROR. `local.ecw_template_build_from_oci_boots`
    hit builder storage admission: it needed 55.83 GB available on the
    builder cache and found 55.59 GB, because `~/.kind-data` still held round
    1's images.
  - `local.telemetry_reaches_local_backends` was SKIPPED in both rounds; it
    needs telemetry mode `otel`, and the specified bench runs `datadog`.
  - **Failure protocol:** this task's round-1 SPACE images were listed in each
    node's containerd and confirmed unused by any container. They were
    created after the owner-authorized wipe, so they're task-owned. Removing
    six image IDs × six nodes freed 11 GB, to 64 GB free. The single rerun
    `space_smoke run local.ecw_template_build_from_oci_boots` then passed.
    Classified ENVIRONMENTAL (disk headroom), so no base comparison was
    needed. Final: ECW 77/77, native 23/23.
- **Log sweep** (56 pods + Tilt log):
  - "tcp connect error": 54,011 pod lines, over 99.8% the Datadog trace
    exporter (no agent with observability off).
  - `ConnectPhaseError`: 108 lines, all from node-ctl's lazy cluster-api
    channel (43 DNS, 65 refused/timed-out dials), at bring-up (21:00) and
    during a scenario-driven control-plane restart (21:22–21:23).
  - `ConnectPhaseTimeout` (the new connect-phase bound): 0, since no lookup
    stalled.
  - "exhausted TCP gRPC connect retry budget": 4, all credential-manager
    agents dialing `space-credmgr-authority` at bring-up. 13 attempts in about
    9.5 s, matching the unchanged default connect policy. Each restarted once
    and stayed up, the same as round 1.
  - The runtime-RPC replay line is debug-level, so it's not visible at info.
- **Teardown:** `teardown.sh` deleted the cluster and stopped Tilt. No Kind
  cluster, container, Tilt or port-forward remains; `/home/dev` has 64 GB
  free.

**Round 1 Kind (`8234955`), for reference.**
- After the owner-authorized removal of `~/.kind-data` (1.4 GB → 86 GB free),
  the baseline passed and the typed suite passed ECW 77/77 and native 23/23
  first time.
- The log sweep had the same shape.

## Decisions

- **Outer bound** (round 1): include the 3 s name-resolution allowance, like
  #360.
- **Inner bound** (round 2): lookup plus dial are bounded together inside the
  connector at 3 s + d. The attempt cap and the whole-connect backstop sit
  0.5 s and 1 s past the TLS bound, so each stall reports its own cause and
  the backstop never fires first.
- **Every routed endpoint gets a dial bound**: 3 s default.
- **Unix-socket detection**: by tonic's fixed fallback URI, the only signal
  tonic exposes. Documented on the constant.
- **`ConnectEndpoint`**: keeps infallible connect signatures, since its
  constructor guarantees both properties. The public raw helpers fail closed.
- **`local_address`**: not carried over (no tonic getter; no SPACE endpoint
  sets one), as documented on the connector.
- **Balanced channel and key-broker channel**: left unmarked. The first has
  no connector hook; the second is not internal transport.
- **Marker `Display`**: transparent, so messages are unchanged.
- **api test channel**: the production pod-client `connect_worker` channel
  with a blackholed redial, because `RuntimeClient::new` is private to the
  substrate module.
- **Typed suite**: run unsharded (`all`), covering every CI shard on one
  cluster.
- **Kind-node images removed before the rerun**: only this task's unused
  round-1 SPACE images.

## Results

| Item | Result |
|---|---|
| Round 1 change (`8234955`) | pushed |
| Review fixes, one commit (`de59314`) | pushed, fast-forward |
| fmt / error policy / clippy (CI flags) / rustdoc | PASS |
| nextest grpc (all features / `internal-tls` / none) | 92/92, 92/92, 84/84 (one pre-existing 1 ms-deadline flake on the first run, unchanged file) |
| nextest grpc + 70 dependents | 7795/7827; the 32 failures are identical to base: ENVIRONMENTAL / PRE-EXISTING |
| Mutations M1–M10, R1–R9 | all caught |
| Stress 300× (grpc 17 tests; api test) | 5100/5100; 300/300 |
| Kind baseline (`de59314`) | PASS |
| Typed Kind suite (`de59314`) | ECW 77/77 (one INFRA_ERROR on disk headroom, passed on rerun), native 23/23 |
| Live log sweep | marker only on node-ctl's cluster-api channel; no unexplained gRPC connect failures |
| Teardown | done |

**Verdict:** PASS. All review findings are fixed in one commit, `de59314`,
with tests proven by mutation and 300-run stress. Validation steps 1 and 2
pass on this box. The only non-passes are environmental: the 32 process-spawn
tests that fail identically on base, and one builder disk-headroom infra
error that passed on its rerun.
