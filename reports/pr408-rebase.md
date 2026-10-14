# PR #408: rebase onto main, #405 watched-path integration, replay metric

Task `task-20261014-031442-7bd289`, 2026-10-14 (UTC).

## Outcome in one paragraph

The branch was rebased onto `9d356f0ac` and an integration commit made
#405's kept (watched) worker channels dial through SPACE's TCP connector.
That was force-pushed with the given lease as `112a699` (milestone 1).
**The owner squash-merged PR #408 from `112a699` at 04:12:59Z as `d7c62c3`**,
and GitHub deleted the branch. Two later pieces of work were not part of that
merge: the owner-requested replay metric, and a test hardening found by
mutation (described below). They now live on a recreated
`refs/heads/ac/grpc-replay-safe-connect-phase`, as two commits on top of
current `main` (`d7c62c3`): `fce53a7` and `8279fec`. No PR exists for them,
because I was told not to touch the PR and did not open one. The Kind bring-up
and the typed suite ran on `8279fec`.

## Commits

| Commit | Where | What |
|---|---|---|
| `fb6d8b7` | merged in `d7c62c3` | `8234955` rebased; conflict resolution only |
| `8715a37` | merged in `d7c62c3` | `de59314` rebased; conflict resolution only |
| `112a699` | merged in `d7c62c3` | integration commit: watched path through the SPACE connector |
| `fce53a7` | branch, on `d7c62c3` | test-only: TLS handshake-bound tests use a 5 s bound |
| `8279fec` | branch, on `fce53a7` | `grpc.client.replay` counter in `RuntimeClient::call` |

**Commit layout:** I kept the two original commits, with conflict resolution
only (each builds alone: `cargo check` of grpc + api, all features, tests).
On top is **one integration commit**. The 5 s test change was first amended
into the integration commit locally (`e7ab0cb`), but that came after the
merge. So on the branch it is a separate commit, `fce53a7`, with the same
diff.

## How the conflicts were resolved

`git rebase 9d356f0ac` stopped twice. No `git merge` was run.
- `crates/libs/common/grpc/src/lib.rs` (both commits): the re-export lists
  were combined. The result has every PR export (`ConnectPhaseError`,
  `TcpChannelError`, `TcpEndpointError`, `connect_tcp_channel`,
  `connect_tcp_channel_lazy`) plus #405's
  `connect_tcp_channel_with_retry_watched`. `mod connection_watch` and
  `pub use ConnectionWatch` merged automatically.
- `crates/libs/common/grpc/src/connect/tests.rs` (commit 1): both sides
  appended tests after the same test. #405's
  `a_watched_dial_applies_tls_and_its_handshake_bound` was kept first, then
  the PR's helpers and tests. The shared closing
  `silent_peer.abort(); }` was restored for each. Every test from both sides
  is kept.
- The second commit's `connect.rs` changes applied cleanly over #405's
  watched function. That is the semantic conflict: the function still used
  #405's own connector.

## Watched-path integration design (`112a699`)

**Choice: `WatchedTcpConnector` wraps `TcpConnector`.** No dial code is
duplicated.
- `connection_watch::WatchedTcpConnector<C>` no longer builds an
  `HttpConnector`. It wraps any connector `C` whose response is
  `TokioIo<T>` and adds only the watch: each stream becomes a
  `WatchedStream` that closes the watch on drop.
- Its `Error` is `C::Error` and `poll_ready`/`call` forward unchanged, so
  `ConnectPhaseError` passes through.
- #405's `ConnectionWatch`/`WatchedStream` are untouched: the watch closes
  when the dialed connection's stream drops, all of a channel's connections
  share one watch, it never reopens, and `close()` still retires.

**Dial.** The new `dial_watched_tcp_channel(endpoint, tls, http)` calls the
same `tcp_connector()` as every other routed channel and wraps its result. So:
- the `HttpConnector` is built by `endpoint_http_connector` (nodelay,
  keepalive, keepalive interval and retries, dial bound);
- lookup and dial are bounded together at `connect_phase_bound(d)` = 3 s + d,
  with every error marked;
- tonic's whole-connect bound is `whole_connect_backstop` = 3 + d + T + 1 s.

It is generic over the resolver only so that a test can inject one that
never answers.

**`connect_tcp_channel_with_retry_watched`** now mirrors
`connect_tcp_channel_with_retry`:
- `check_tcp_endpoint` first: `TcpEndpointError::MissingTlsHandshakeBound`
  or `UnixSocket` before any dial;
- the default dial bound (3 s) for an endpoint without one;
- `tcp_attempt_cap(d, T)` = 3 + d + T + 0.5 s.

The resulting order: lookup+dial (3+d), TLS (T), `AttemptTimeout` (+0.5 s),
backstop (+1 s). #405's `dial + TLS` whole-connect bound is gone.

`ConnectEndpoint::connect_with_retry_watched` already passed
`self.tls_handshake_timeout` and is unchanged. A new test, and mutation W6,
prove it.

## Every dial #405 added or changed, after the rebase

I compared a grep of every channel construction (`.connect()`,
`.connect_lazy()`, `connect_with_connector`, `balance_channel`,
`connect_with_retry*`, `connect_rotating`, `connect_tcp_channel*`,
`Endpoint::from*`, `TcpStream::connect`) across non-test code, between
`de59314` and the rebased tree. The only new dial sites were #405's watched
ones. The net-gateway commits in the range add TCP proxy code, not gRPC
channels.
- `kept_worker_client` → `connect_worker_watched` →
  `ConnectEndpoint::connect_with_retry_watched` →
  `connect_tcp_channel_with_retry_watched`: **now routed** (was #405's
  unmarked connector).
- `PodWorkerChannels::client`: caches the result of that dial; no dial of
  its own.
- Retire paths (`retire_after_transport_failure`, `retire_after_call_failure`,
  `reconnect_node_api`, `RuntimeClient::retire_kept_connection`): only
  `watch.close()`. The reconnect redials via `connect_spaced_runtime_client` →
  substrate → `kept_worker_client` (routed) or the direct `WorkerChannels`
  (routed).
- `pod_worker_channel` / `pod_worker_client` / `pod_worker_internal_client`
  (readiness and internal paths, which keep dialing fresh): via
  `node_api_connect_endpoint(..).connect()` = `ConnectEndpoint::connect`,
  routed.
- `worker_client_for_pod_ip` → `connect_worker` → `connect_with_retry`:
  routed.
- `pod_proxy_accepts_tcp`: a raw `TcpStream::connect` reachability probe, not
  an RPC channel; unchanged from before #405.

## Caller table (bounds after the rebase)

`d` = endpoint dial bound; `T` = TLS handshake bound (15 s internal https;
none for dev plaintext); phase = 3+d; backstop = 3+d+T+1.

| Caller | Connect path | Dial / phase / attempt cap / backstop (https; http) |
|---|---|---|
| **api kept pod worker channels (#405)**: `kept_worker_client` → `connect_worker_watched` → `ConnectEndpoint::connect_with_retry_watched` | naive rebase: #405's own `HttpConnector`, unmarked, no lookup bound, whole-connect `d+T` (18 s; 3 s) → **SPACE `TcpConnector` + watch layer, first dial and every redial** | 3 s / 6 s / 21.5 s; 6.5 s / 22 s; 7 s. Whole call (default policy): 31.5 s; 16.5 s |
| `connect_tcp_channel_with_retry` via `ConnectEndpoint::connect_with_retry` (api `connect_worker`/`_internal`/`_builder`, pod-ops internal client, build-mgr `connect`) | SPACE connector per attempt and reconnect | d (3 s default) / 6 s / 21.5 s; 6.5 s / 22 s; 7 s. Whole call 31.5 s; 16.5 s |
| credmgr client (Tcp, Tls) via `connect_tcp_channel_with_retry` | same | 3 s / 6 s / Tls 9.5 s, Tcp 6.5 s / Tls 10 s, Tcp 7 s |
| `connect_tcp_channel_with_retrying_redials` (api direct `WorkerChannels`, a=3 s) | `TcpConnector` per attempt, final error marked | 3 s / 6 s / 6.5 s per attempt / `retrying_redial_bound` 32.5 s; 17.5 s |
| Data-plane backend pool, ingress activation (lazy) | `ConnectEndpoint::connect_lazy` | 3 s / 6 s / — / 22 s; 7 s |
| node-ctl cluster-api client (lazy) | same | 5 s / 8 s / — / 24 s; 9 s |
| Transfer source (one-off) | `ConnectEndpoint::connect` | 10 s / 13 s / — / 29 s; 14 s |
| `pod_worker_channel` (one-off; readiness keeps dialing fresh after #405) | `node_api_connect_endpoint` + `connect` | c / 3+c / — / 3+c+T+1 |
| Admin node-maintenance verify (one-off) | `ConnectEndpoint::connect` | 5 s / 8 s / — / 24 s; 9 s |
| `NodeApiClient` connect closure | `transport.connect_endpoint(..).connect()` | d / 3+d / `GrpcClient`'s 1 s attempt cap / 3+d+T+1 |
| Cluster-api discovery probe, `RotatingChannel` | `connect_endpoint(..).connect()` / `.connect_rotating(..)` | 5 s / 8 s / — / 24 s; 9 s |
| Build-mgr lazy client | `connect_endpoint` + `connect_lazy` | 3 s default / 6 s / — / 22 s; 7 s |
| Not routed: tonic balanced channel (grpc-lb), credmgr key-broker channel, observability trace-parity fixture (test), all UDS/vsock | — | unchanged, unmarked |

## Replay metric (`8279fec`, owner-requested)

- **Metric:** `grpc.client.replay` (counter, 1 per replay decision).
  - Helper `pplx_space_metrics::grpc::client_replay(service, cause, outcome)`.
  - Vocabulary `ClientReplayOutcome`, declared with the crate's
    `metric_vocabulary!`.
  - Documented on the helper and the enum in `crates/libs/metrics/src/grpc.rs`,
    next to `grpc.client.retry` and `grpc.client.channel_cache`. The metrics
    crate's module docs are the catalog. No docs page lists `grpc.client.*`
    counters, so none needed updating.
- **Tags** (all bounded, no ids):
  - `service`: `worker-node-api`, as the worker dials and
    `grpc.client.channel_cache` use.
  - `cause`: `connect_phase` | `refused` | `not_found`.
  - `outcome`: `replayed_ok` | `replayed_error` | `no_fresh_client` (the probe
    returned none or failed) | `deadline_expired` (the deadline was checked
    or fired before or during the replay).
  - A call its caller abandons counts nothing.
- **Cause without a second classifier:** `pplx-space-common-grpc` now exports
  `ClientConnectFailureCause` and `client_connect_failure_cause(&Status)`,
  which is the old chain walk returning which proof it met.
  `is_client_connect_failure` is now `client_connect_failure_cause(..).is_some()`,
  so the decision and the tag cannot diverge.
  - A refusal through SPACE's connector reports `connect_phase`, because the
    marker sits above the io error. `refused`/`not_found` come only from dials
    outside it (UDS, tonic's own connector).
- `RuntimeClient::call` keeps its debug log and its control flow. The probe's
  `?` became an explicit match so that the outcome is recorded; it returns
  the same errors.
- No monitors, dashboards or production telemetry were added.
- **Tests:**
  - The api blackhole test now also asserts exactly one
    `grpc.client.replay{cause:connect_phase,outcome:replayed_ok,service:worker-node-api}`.
  - New `a_never_sent_rpc_without_a_fresh_client_counts_its_replay_decision`:
    a real refused status, Direct substrate with a closed pool →
    `cause:refused, outcome:no_fresh_client`, 1 attempt.
  - `peer_errors_are_returned_without_replaying_the_request` asserts no
    counter.
  - The refused-dial test asserts `connect_phase` for SPACE's connector and
    `refused` for tonic's.
  - All use the thread-local `test_capture` recorder.

## Tests and mutations

**New or changed grpc tests (`connect/tests.rs`):**
- `a_watched_channels_connect_phase_timeout_is_a_client_connect_failure`
  (**the requested test**): a watched channel serves a call. The peer stops,
  so the watch closes. The address is blackholed. The held channel's next RPC
  redials through the watched connector and fails with a `TimedOut` that
  carries the marker and that `is_client_connect_failure` accepts. The watch
  stays closed.
- `a_watched_dial_that_never_connects_fails_in_its_connect_phase` (paused
  clock): the first dial fails as marked `Transport`, not `AttemptTimeout`.
- `a_watched_dials_stalled_lookup_fails_with_its_own_cause` (paused clock,
  stalled resolver): marked, exactly at `connect_phase_bound(d)`.
- `a_watched_internal_rpc_connect_fails_at_its_transports_handshake_bound`:
  `ConnectEndpoint::connect_with_retry_watched` uses the transport's bound.
  The existing internal-RPC test now shares the new
  `internal_rpc_connect_endpoint` helper.
- `a_tcp_connect_refuses_endpoints_it_would_dial_wrongly` now also covers the
  watched helper.

**#405's tests** (all pass): the watched-channel close, clone-close and
closed-port tests, `a_watched_dial_applies_tls_and_its_handshake_bound`, the
six `pod_worker_channels` tests and
`spaced::tests::a_kept_connection_is_retired_after_a_timeout_or_transport_close_only`.

**Mutations.** Each was applied alone in a separate temporary worktree, the
whole grpc crate was run (plus the named api tests), then reverted. The
earlier set was reconstructed from the round-2 report's descriptions;
M1≡R8, M4≡R9 and M6≡R6 are one mutation each on the current code.

| Mutation | Caught by |
|---|---|
| M1/R8 `TcpConnector` drops the marker | lazy timed-out dial, default dial bound, both stalled-lookup tests, the 3 new watched tests; **api blackhole test** |
| M2 classifier accepts raw `TimedOut` | unmarked-timeout test, local-chain test |
| M3 classifier ignores the marker | kept-redial, lazy timeout, default-bound, stalled-lookup, **watched connect-phase test** |
| M4/R9 whole-connect bound = dial bound | 4 handshake-bound tests, lazy TLS test, both stalled-lookup tests, watched stalled lookup |
| M5 retrying connector unmarked | kept-redial test |
| M6/R6 any local io kind | unmarked-timeout, local-chain |
| M7 message-text fallback | peer-UNAVAILABLE test, local-chain test |
| M8 any tonic transport error | lazy TLS, silent-connection, unmarked-timeout |
| M9 any local chain, no code gate | 5 tests |
| M10 no code gate + any io kind | 3 tests |
| R1 no inner connect-phase bound | both stalled-lookup tests + watched stalled lookup |
| R2 no default dial bound | kept-redial, default-bound |
| R3 no fail-closed check | refusal test |
| R4 no Unix-socket detection | refusal test, internal-transport unix test |
| R5 `ConnectionReset` counts | local-chain test |
| R7 any local UNAVAILABLE | 4 tests |
| **W1 watched connector drops the marker** (keeps the io `TimedOut`) | **watched connect-phase test**, watched first dial, watched stalled lookup |
| W2 watched dial bypasses `TcpConnector` (naive rebase) | same 3 |
| W3 watched helper skips the endpoint check | refusal test |
| W4 watched whole-connect bound = dial+TLS (#405's) | watched stalled lookup |
| W5 watched attempt cap omits TLS | watched TLS tests (10/10 after `fce53a7`; 2/5 before) |
| W5u unwatched attempt cap omits TLS | unwatched TLS tests (10/10 after `fce53a7`) |
| W6 `connect_with_retry_watched` passes no bound | watched internal-RPC test (10/10) |
| W7 watch never closes | #405 close test, **watched connect-phase test** |
| X1 no replay counter | api blackhole test, no-fresh-client test |
| X2 cause always `connect_phase` | no-fresh-client test |
| X3 probe failure counted `replayed_error` | no-fresh-client test |
| X4 replay outcome always `replayed_error` | api blackhole test |
| X5 classifier calls refusals `connect_phase` | refused-dial test, no-fresh-client test |

All 29 were caught.

**Test hardening found by mutation (`fce53a7`).** W5 was caught only 2 of 5
times.
- The handshake-bound tests used T = 4 s with d = 0.5 s. A cap without TLS is
  3 + 0.5 + 0.5 = 4 s, which equals T, so whether the attempt or the handshake
  fired first was a race.
- With T = 5 s, W5, W5u, W6 and M4 each failed 10 of 10 stress runs. The
  unwatched twin had the same latent race since `de59314`.
- This changes the bound in 4 tests and one comment. No production code
  changes.

**Stress (nextest `--stress-count 300`):**
- 10 watched/changed grpc tests on `112a699`: 300/300 iterations, 3000 runs,
  0 failures (1210 s).
- The 4 handshake-bound tests at 5 s: 300/300 iterations, 1200 runs,
  0 failures (1512 s).

## Validation

**Step 1** ran on `112a699`, then again on `bdfbaa5` (local, pre-merge
layout) and on the final `8279fec`. Each run was fmt, error policy, every CI
clippy invocation (`-D warnings`), grpc no-features clippy, `cargo doc -p
pplx-space-common-grpc` with `-D warnings`, grpc nextest (all features /
`internal-tls` / none), and grpc + 70 dependents + db-test-fixtures
(`--all-features --retries 1 -E 'not group(privileged)'`).
- Final: PASS; grpc 100/100, 100/100, 90/90.
- Dependents: 7947 run, 7915 passed, **32 failed**. The 32 are the spaced
  guest-server process tests and one host-cli test (`PermissionDenied`, os
  error 13). The same 32 fail on main `9d356f0ac` on this box: identical
  list, 263 run, 231 passed. **ENVIRONMENTAL / PRE-EXISTING.**
- The metrics crate's own rustdoc with `-D warnings` fails on two
  pre-existing links in `sandbox/api.rs` and `template.rs` (identical on
  `9d356f0ac`; not touched). It passes with only those two lints allowed, so
  the new docs add no warnings. **PRE-EXISTING.**
- Prerequisites: `protoc` 35.0 was installed the way CI's `install_protoc`
  does; the dangling `aws` link was replaced via
  `infra/space/kind/scripts/install-prereqs.sh`, as CI does.

**Step 2 (Kind).**
- A first bring-up on `112a699` passed its baseline (03:53–04:03). I stopped
  its typed suite during scenario 2 when the owner's metric request arrived,
  and tore the cluster down, so that the Kind run would cover the metric.
- Disk: Docker root was below 20 GiB twice. Following the authorization, I
  listed and removed older `space-*:tilt-*` tags, keeping the newest per repo:
  28 tags, then 14. No chef-base/rust-build image was superseded. Docker root
  ended at 11 GiB free; nothing else qualified. `/home/dev` stayed above
  100 GB free.
- **Final bring-up on `8279fec`:** `smoke-kind-e2e.sh` with the specified env
  passed (rc=0, 04:42–04:53). The api, worker, cluster-controller and
  data-plane images carry `org.opencontainers.image.revision=8279fece8475`.

- **Typed suite** (`SKIP_BOOTSTRAP=1 smoke-kind-typed.sh all`, unsharded so
  it covers every CI shard on one cluster, with the CI shard env: admin token
  file, cold receipts dir, `uv` from `ensure-uv.sh`, data-plane health gate),
  04:54–05:43 UTC, rc=0:
  - ECW surface **77/77**. Native surface **23/23**. No failures, so no
    reruns were needed.
  - `local.telemetry_reaches_local_backends` was SKIPPED, because it needs
    telemetry mode `otel` and the specified bench runs `datadog` (same as the
    previous round).
- **Log sweep** (76 pods):
  - `tcp connect error`: 52,403 lines, almost all the Datadog trace exporter
    (`libdd_trace_utils::send_with_retry`; no agent with observability off).
  - `ConnectPhaseError`: 118 lines, all DNS lookups on the node-ctl and
    builder-node-ctl cluster-api channels during scenario-driven
    control-plane restarts.
  - The connect-phase bound's own timeout ("name resolution and TCP dial did
    not finish"): 0. "exhausted TCP gRPC connect retry budget": 0.
    `panicked at`: 0.
  - `grpc.client.replay` is not observable here: observability is off, and
    the replay log is debug-level. The api test proves it.
- **Teardown:** `teardown.sh` stopped Tilt and deleted the cluster (rc=0). No
  Kind cluster, Tilt process, port-forward or SPACE container remains. My
  temporary mutation worktree, its target dir and its branch were removed.

## Results

| Item | Result |
|---|---|
| Rebase onto `9d356f0ac`, conflicts (lib.rs, connect/tests.rs) | resolved; both sides' exports and tests kept |
| Integration: watched path through SPACE `TcpConnector` (`112a699`) | done; force-pushed with the given lease (milestone 1) |
| PR #408 | **merged by the owner from `112a699`** as `d7c62c3` (04:12:59Z) |
| Follow-ups on recreated branch (`fce53a7`, `8279fec`, on `d7c62c3`) | pushed; no PR opened |
| Replay metric `grpc.client.replay` (service, cause, outcome) | added, documented in `crates/libs/metrics/src/grpc.rs` |
| fmt / error policy / clippy (CI flags) / grpc rustdoc | PASS (final HEAD) |
| nextest grpc (all / `internal-tls` / none) | 100/100, 100/100, 90/90 |
| nextest grpc + 70 dependents | 7915/7947; the 32 failures are identical on `9d356f0ac`: ENVIRONMENTAL / PRE-EXISTING |
| metrics-crate rustdoc `-D warnings` | 2 PRE-EXISTING broken/private links (unchanged files); new docs clean |
| Mutations (16 earlier, 8 watched-path, 5 metric) | 29/29 caught |
| Stress 300× | 3000/3000 and 1200/1200 runs |
| Kind baseline (`8279fec`) | PASS |
| Typed Kind suite (`8279fec`) | ECW 77/77, native 23/23 |
| Teardown | done |

**Verdict:** PASS. The rebase, the watched-path integration and the replay
metric are done and validated: steps 1 and 2 pass on the final HEAD
`8279fec`, and the only non-passes are pre-existing or environmental. One open
item for the owner: PR #408 merged from `112a699` before the metric
arrived, so `fce53a7` and `8279fec` are on
`ac/grpc-replay-safe-connect-phase` (based on current `main`) and need a new
PR to land.


## Decisions

- **Base:** `9d356f0ac` as authorized. Later main commits merged cleanly
  (`merge-tree`). After the squash-merge, the follow-up commits are on
  `d7c62c3`.
- **Recreated branch:** the environment contract requires the final HEAD on
  this exact ref. I recreated it with `--force-with-lease=<ref>:` (must not
  exist), after the authorized lease no longer applied. Nothing on main was
  rewritten. No new PR, comment or body edit.
- **Composition:** the watch is a wrapper over `TcpConnector`, not an option
  inside it. `TcpConnector` stays the single place that dials and marks, and
  the watch stays in #405's module.
- **`cause` tag kept:** it comes from the exported classifier, not a copy.
  The metrics crate takes `cause` as the classifier's `as_str()` because it
  cannot depend on `pplx-space-common-grpc` (that crate depends on it).
- **No api-level twin for the kept path:** `connect_worker_watched` only
  forwards to `connect_with_retry_watched`, which the grpc tests cover
  directly. The api blackhole test uses the direct `connect_worker`.
