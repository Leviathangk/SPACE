# TCP connect-phase failures replay-safe via a typed SPACE marker

Feature branch `ac/grpc-replay-safe-connect-phase` at `8234955` (one commit on
`main` `64afd0b`). No pull request was opened.

## Design

**Marker.** `pplx_space_common_grpc::ConnectPhaseError` is a public
`thiserror` type that wraps a boxed error. Its `Display` is the source's
message and `source()` is the original error, the same convention as tonic's
own `ConnectError`. Because of that, status messages are unchanged
(`"tcp connect error"`) and the existing `ConnectionRefused`/`NotFound` check
still finds its `io::Error` below the marker. Only the connector can build
one: the constructor is private, so other code cannot mark an arbitrary error
as replay-safe.

**Connector.** `connect.rs` has a private `TcpConnector` that wraps
hyper-util's `HttpConnector` and maps every `poll_ready` and `call` error to
`ConnectPhaseError`. That covers name resolution, refused, timed-out and
unreachable dials, and URI errors: everything that happens before a stream
exists. The inner connector is configured the way tonic 0.14.6's
`Endpoint::http_connector()` configures it: `enforce_http(false)`, plus
nodelay, keepalive, keepalive interval/retries and `connect_timeout`, all read
from the `Endpoint` getters. One gap: tonic has no getter for `local_address`,
so the connector keeps the default (the kernel chooses). No SPACE endpoint
sets one; I checked with `grep`. Public entry points:
`connect_tcp_channel(endpoint, tls_handshake_timeout)`,
`connect_tcp_channel_lazy(..)`, and on `internal_transport::ConnectEndpoint`
the new `connect()`, `connect_lazy()` and `connect_rotating()`. The
`ConnectEndpoint` methods supply the TLS bound their transport applied, so
callers never build a connector themselves.

The retrying-redial connector now builds its `HttpConnector` with the same
helper, then overrides the dial bound with `attempt_timeout`. When retries
run out it wraps the final error, a dial error or an `AttemptTimeout` (an
attempt abandoned during lookup or dial), in `ConnectPhaseError`. Its
nodelay/keepalive used to be hard-coded. They now come from the endpoint.
That is equivalent for its only caller, whose endpoint sets nodelay=true and
no keepalive, the values that were hard-coded.

**The `TimeoutConnector` trap.** `connect_with_connector[_lazy]` wraps the
whole connector, TLS included, in tonic's `TimeoutConnector`, using the
endpoint's `connect_timeout`. `connect()` gives that value only to the dial.
`tcp_connector()` therefore:
1. puts the endpoint's dial bound `d` on the inner `HttpConnector`, and
2. if the endpoint has a dial bound, replaces the endpoint's `connect_timeout`
   with `tcp_attempt_cap(d, tls) = 3 s name-resolution allowance + d + TLS
   handshake bound`. This follows the pattern from #360, whose
   `retrying_redial_bound` also includes the allowance. An endpoint with no
   dial bound gets no outer bound at all, exactly as with `connect()`.

The dial keeps its own bound, and the TLS handshake keeps its own
(`ClientTlsConfig::timeout`, 15 s internal). The only new limit is on a whole
connect whose lookup overruns 3 s plus whatever the dial and handshake left
unused. Before, such a connect was unbounded, because nothing bounded name
resolution. If the outer bound fires, it surfaces as an unmarked
`TimedOut`/`Unknown`, which is conservatively not replay-safe. Mutation M4
below shows that cutting the outer bound back to `d` breaks three tests.

**TLS.** tonic layers TLS over the connector's stream, so TLS errors
(including `HandshakeTimeout`) never carry the marker and stay not
replay-safe. That is intended, and a test documents it.

**Classifier.** At each level of the chain, `is_client_connect_failure` now
returns true when it finds `ConnectPhaseError`, before the existing
first-`io::Error` check. The `Unavailable` code gate and the
refused/not-found check are kept. A bare `TimedOut` without the marker stays
false. The doc comment explains why: the kernel also reports ETIMEDOUT for an
established connection whose retransmissions give up.

Observed chain for a timed-out lazy dial:
`Status{Unavailable,"tcp connect error"} → transport::Error → tonic ConnectError → ConnectPhaseError → hyper-util ConnectError("tcp connect error") → io::Error(TimedOut)`.

## Caller table

`d` = the endpoint's dial bound. `T` = TLS handshake bound: 15 s for internal
https, none for dev plaintext. "Whole" = the bound on one complete connect,
lookup through TLS. Before this change, name resolution was unbounded on
every `connect()`/`connect_lazy()` path.

| Caller | Before | After | Dial / whole (https; http) |
|---|---|---|---|
| `connect_tcp_channel_with_retry`, used via `ConnectEndpoint::connect_with_retry` (api `connect_worker`/`_internal`/`_builder`, pod-ops internal client, build-mgr `connect`) and directly by credmgr client Tcp/Tls | per attempt `endpoint.connect()`; attempt cap 3+d+T; later reconnects: dial d + TLS T | per attempt `connect_tcp_channel`; same attempt cap and budget; every dial, reconnects included, bounded at 3+d+T | d (3 s default) / 3+d+T, e.g. 21 s; 6 s. credmgr: T = its TLS config's bound |
| `connect_tcp_channel_with_retrying_redials` (api `WorkerChannels`, d=3 s) | raw `HttpConnector`, unmarked errors | endpoint-configured `HttpConnector`, final error marked | unchanged: attempt 3 s, whole `retrying_redial_bound` |
| Data-plane backend pool (`state.rs`, lazy) | `client_endpoint` + `connect_lazy` | `client_connect_endpoint` + `connect_lazy` | 3 s / 21 s; 6 s |
| Ingress activation (`activation.rs`, lazy) | `transport.endpoint` + `connect_lazy` | `connect_endpoint` + `connect_lazy` | 3 s / 21 s; 6 s |
| node-ctl cluster-api client (`node/ctl/server`, lazy) | `client_endpoint` + `connect_lazy` | `client_connect_endpoint` + `connect_lazy` | 5 s / 23 s; 8 s |
| Transfer source (`transfer.rs`, one-off) | `client_endpoint` + `connect` | `client_connect_endpoint` + `connect` | 10 s / 28 s; 13 s |
| `pod_worker_channel` (`worker_clients.rs`) | `node_api_transport` + `connect` | `node_api_connect_endpoint` + `connect`; the now-unused `node_api_transport` was removed | caller's c / 3+c+T |
| Admin node maintenance verify | `client_endpoint` + `connect` | `client_connect_endpoint` + `connect` | 5 s / 23 s; 8 s |
| `NodeApiClient` connect closure (trigger fanout d=2 s, substrate worker d=3 s, sandbox-service-ports d=3 s, …) | `transport.endpoint(..).connect()` | `transport.connect_endpoint(..).connect()` | d / 3+d+T; `GrpcClient`'s 1 s connect-attempt timeout still caps the eager connect (unchanged) |
| Cluster-api client: discovery probe connect + `RotatingChannel` first dial and rotations (`cluster.rs`) | `endpoint.connect()` | `connect_endpoint(..).connect()` / `.connect_rotating(..)`; `RotatingChannel::connect` gained a `tls_handshake_timeout` argument | 5 s / 23 s; 8 s |
| Build-mgr lazy client | `endpoint` + `connect_lazy`, no `connect_timeout` | `connect_endpoint` + `connect_lazy` | unbounded dial, no whole bound (unchanged) |
| Not routed: tonic balanced channel (grpc-lb, cluster-api discovery) | tonic-internal `http_connector()` | unchanged (no connector hook in `Channel::balance_channel`) | unchanged, unmarked |
| Not routed: credmgr intermediate key-broker channel (`run.rs`), not internal transport; observability trace-parity fixture (test); all UDS/vsock channels | — | unchanged | — |

## What each `is_client_connect_failure` caller now replays

Newly classified as never sent: any failure from the SPACE connector, meaning
a dial that hit its bound, EHOSTUNREACH/ENETUNREACH, a DNS failure, a bad
URI, and, on kept worker channels, a redial whose retries ran out (including
abandoned attempts). Retry budgets, backoff and RPC selection are unchanged.

| Caller | Newly replayed or reclassified | Budget that bounds it |
|---|---|---|
| api `RuntimeClient::call` (`runtime_rpc.rs`), on node-api TCP channels | One replay on a freshly probed client after such a failure. Before, only a refused dial earned it. | Unchanged: at most one replay, all inside `timeout_at(deadline)`, with each attempt's grpc-timeout set to the remaining deadline |
| api `classify_access_idle_send_failure` (`updates.rs`) | Such a failure is now `Untransmitted`: it returns the paced 503 `unconfirmed_access_idle_update` (retry-after 1 s) instead of `Ambiguous`, which did one State observation. It never resends. | Unchanged; it runs after `RuntimeClient::call`'s single replay, within the same deadline |
| node-api `restart_handoff` | Nothing: it forwards to sandbox-manager over UDS, which never carries the marker | Unchanged |
| node-mgr `runtime_supervisor_client` | Nothing: the runtime supervisor is on a node-local Unix socket | Unchanged |

## Tests (all in `crates/libs/common/grpc/src/connect/tests.rs`)

I refactored the full-accept-queue fixture (`listen(2)` plus three queued
connections) into `Blackhole` and reused it in the existing
`an_endpoint_without_a_dial_bound_is_redialed_at_the_default_one`. New tests:

1. `a_lazy_channels_timed_out_dial_is_a_client_connect_failure`: paused clock,
   blackhole, a real health RPC on `connect_tcp_channel_lazy`. The chain has
   `TimedOut` and the marker, and the classifier says true.
2. `an_unmarked_timed_out_dial_is_not_a_client_connect_failure`: the same dial
   through tonic's own `connect_lazy`. The chain has a bare `TimedOut`, and the
   classifier says false.
3. `a_refused_dial_is_a_client_connect_failure_through_either_connector`:
   true through both connectors (existing behaviour).
4. `a_kept_channels_redial_that_never_connects_is_a_client_connect_failure`:
   a retrying-redial channel whose peer stops and whose address becomes a
   blackhole. The RPC's chain has `TimedOut` and the marker, and the
   classifier says true.
5. `an_rpc_whose_connection_dies_after_it_was_sent_is_not_a_client_connect_failure`:
   a peer reads the preface and frames up to the request HEADERS (counted),
   then closes or resets (`set_zero_linger`). The failure has a local source
   chain but the classifier says false. The observed status is
   `Unknown "transport error"` with hyper `Io(ConnectionReset)`.
6. `a_peers_unavailable_that_reads_like_a_connect_failure_is_not_one`: a real
   server returns `UNAVAILABLE` with refused / timed-out / DNS wording. The
   code and message arrive intact, and the classifier says false.
7. `a_lazy_internal_rpc_channels_tls_handshake_timeout_is_not_a_client_connect_failure`
   (`internal-tls`): a silent TLS peer reached via
   `ConnectEndpoint::connect_lazy` with a 1 s handshake bound and a 0.5 s dial
   bound. It fails with "TLS handshake timeout" after at least 1 s, carries no
   marker, and the classifier says false.

No test pins a configuration value.

Mutation results (each mutation applied alone, then reverted; the selected
tests were run each time):

| Mutation | Failing tests |
|---|---|
| M1 `TcpConnector` returns unmarked errors | 1 |
| M2 classifier accepts raw `TimedOut` | 2 |
| M3 classifier ignores the marker | 1, 4 |
| M4 trap: whole connect bounded at the dial bound | 7, plus the existing `a_tls_peer_that_never_answers_…` and `an_internal_rpc_connect_fails_at_its_transports_handshake_bound` |
| M5 retrying-redial connector returns unmarked errors | 4 |
| M6 any local `io::Error` kind counts | 2 |
| M7 text-matching fallback on the message | 6, plus the existing `client_connect_failure_requires_a_local_error_chain` |
| M8 any tonic `ConnectError` (TLS included) counts | 7, 2 |
| M9 any local error chain counts | 5, 2, 7 |
| M10 no code gate and any `io::Error` kind | 5, 2 |

M6 alone does not trip test 5. Sent-then-died failures surface as `Unknown`,
so the `Unavailable` gate rejects them, and M9/M10 are what cover test 5.

Stress: 300 runs of each new test (plus the refactored existing one) on the
final code: 300/300 iterations passed (2400/2400 test runs, 0 failures, 310 s), on HEAD `8234955`.

## Validation

**Step 1 (pushed as milestone 1).**
- `cargo fmt --all -- --check`, `check-rust-error-policy.sh` (the first draft
  hand-implemented `Error` and was switched to `thiserror`), and the env
  contract check all pass.
- Every clippy invocation from `run-rust-checks.sh clippy` passes with
  `-D warnings`: workspace `--all-targets --all-features`, observability otel,
  common-svc and feature-flags with no default features. So does grpc with no
  features.
- `RUSTDOCFLAGS=-D warnings cargo doc -p pplx-space-common-grpc` and the
  doctests pass.
- nextest `pplx-space-common-grpc`: 78/78 with no features and 85/85 with
  `internal-tls`.
- nextest for grpc and all 70 workspace dependents (plus db-test-fixtures,
  which the Postgres setup script needs), `--all-features --retries 1
  -E 'not group(privileged)'`: 7819 run, 7787 passed, 32 failed. All 32 are
  spaced guest-server `process::*` tests plus one spaced host-cli `output`
  test. They fail with `PermissionDenied (os error 13)` when spawning a
  process, and the identical 32 fail on base `64afd0b` in a temporary
  worktree. Classified **ENVIRONMENTAL / PRE-EXISTING**: this box's
  process-spawn permissions, unrelated to TCP gRPC.
- Environment notes: builds used a target dir on the container's root
  overlay with debuginfo off, because `/home/dev` was 100% full.

**Step 2 (Kind): PASS.**

*First attempt (blocked).* `/home/dev` had 1.4 GB free (393 GB disk; 295 GB
other sessions' worktree `target/` dirs, 84 GB the preserved `~/.kind-data`
node containerd cache). The Docker daemon sees only that shared disk (a
bind-mount probe of the container's `/var/tmp` came back empty), and a
bring-up was estimated to need 8–16 GB there. Freeing it was not authorized,
so the first report recorded an ENVIRONMENTAL blocker. Within the original
scope: Docker root was 6.7 GB free (< 20 GiB), so 187 stale `space-*:tilt-*`
tags were listed and removed, keeping the newest image per repo (no
`space-rust-chef-base`/`space-rust-build-kind` image was superseded); Docker
root then had 22 GB free.

*Disk, with the box owner's later authorization.* Confirmed first:
`kind get clusters` found none, no Kind node containers existed, and no
`tilt up` process ran. Then `~/.kind-data` was removed entirely; nothing else
was touched. `df -h /home/dev`: before 393G size / 392G used / 1.4G avail
(100%); after 393G / 308G / 86G avail (79%), above the 30 GB threshold.

*Build shape.* No separate rebuild was needed. The debuginfo-off settings
applied only to the host nextest target dir, which had already been deleted.
The Kind images come from `build-rust-artifacts.sh` with its own settings:
incremental mode, `cargo --release`, its own target and cargo home under
`~/.space-devbox/kind`, and the CI-identical artifact layout. The bring-up
shell did not set any `CARGO_*` overrides. The first artifact build synced
121 changed sources and compiled for 175 s, the glibc guard passed, and every
deployed image carries revision `8234955707b7`.

*Prerequisites, as CI does them.* `install-prereqs.sh` with
`INSTALL_DIR=~/.local/bin` installed the pinned AWS CLI; kind, kubectl, helm,
grpcurl and tilt were already at their pins. `space_smoke_prepare_encrypted_targets`
reported the host dm-crypt and dm-snapshot targets ready, with the modules
already loaded, so no host change was made. `/dev/kvm` is accessible to the
`kvm` group.

*Baseline.* `BOOTSTRAP_TIMEOUT=1800 DATA_PLANE_URL=http://127.0.0.1:8090
SPACE_KIND_OBSERVABILITY=off SPACE_KIND_TELEMETRY_MODE=datadog bash
scripts/tests/smoke-kind-e2e.sh` ran from a fresh cluster (18:22–18:29 UTC)
and passed: `[7/7] PASS round-trip ok`, exit 0.

*Typed suite.* `SKIP_BOOTSTRAP=1 bash scripts/tests/smoke-kind-typed.sh all`
ran unsharded, i.e. all four CI shards' scenarios, on that cluster. The
environment matched the CI shard step: an admin session token file from
`root@local`, a fresh 0700 cold-receipts dir, uv from `ci/scripts/ensure-uv.sh`
on PATH, aws/curl/jq present, and a data-plane `/healthz` wait. It ran
18:30–19:19 UTC with exit 0:
- ECW API surface: 77 passed, 0 warned, 0 failed, 0 inconclusive.
- Native API surface: 23 passed, 0 warned, 0 failed, 0 inconclusive.
- No scenario was quarantined.

No failures, so no rerun or base comparison was needed.

*Connect failures and replays in the logs, captured while the stack was up.*
Sources: all 56 `space` pods' logs and the Tilt log for this run. No pod had
a previous container still retained.
- "tcp connect error": 52,286 pod-log lines (64,174 in the Tilt log).
  - More than 99.8% are the Datadog trace exporter (libdd `send_with_retry`
    / "Error sending traces"). It is an HTTP client, not a gRPC channel, and
    is refused because no Datadog agent runs with
    `SPACE_KIND_OBSERVABILITY=off SPACE_KIND_TELEMETRY_MODE=datadog`. Every
    service shows it, and it is unrelated to this change.
  - The gRPC remainder comes from node-ctl's lazy cluster-api channel, which
    now goes through `ConnectEndpoint::connect_lazy`, as "outbound facet sync
    failed … cluster node state publish failed". These lines confirm the
    marker in production code: the logged source chains read
    `tonic::transport::Error → ConnectError → ConnectPhaseError → hyper-util
    ConnectError → io`, for DNS lookup failures (98), refused dials (70) and
    timed-out dials (2). They occur only at 18:27 (cluster-api starting
    during bring-up) and 18:51–18:52 (a scenario-driven control-plane
    restart), and the sync then recovered.
  - `ConnectPhaseError` appears only in the four node-ctl pods' logs (191
    occurrences).
- "exhausted TCP gRPC connect retry budget": 4 lines, all at 18:27:35–36
  during bring-up. They come from the four credential-manager agents dialing
  `space-credmgr-authority` before it served. Each made 13 attempts in about
  9.7 s, which is exactly the unchanged `DEFAULT_GRPC_CONNECT_RETRY_POLICY`
  (12 retries, 10 s). Those four pods restarted once (exit 1 at 18:27:40) and
  stayed up for the rest of the run; these were their only restarts. This is
  bring-up ordering, not a suite failure, and was not compared with base.
- "cluster api connect failed": 40 lines from sandbox-, template- and
  volume-manager, all at 18:27 or 18:51–18:52, i.e. the same cluster-api
  start and restart windows.
- Runtime-RPC replay line ("runtime RPC was not transmitted; retrying on a
  fresh worker connection"): 0. It is logged at debug level, and the pods run
  at info, so replays are not observable at this log level. The typed suite's
  worker-restart and recovery scenarios all passed.

*Teardown.* `infra/space/kind/scripts/teardown.sh` stopped the Tilt watcher
and deleted cluster `space-local` with all six nodes. Afterwards no Kind
cluster, Kind container, Tilt or port-forward remained. `/home/dev` was left
at 64 GB free.

## Decisions

- **Outer bound.** It includes the 3 s name-resolution allowance, matching
  #360, so the dial and the handshake each keep their own bound.
- **`local_address`.** Left at the default, because tonic has no getter and
  no SPACE endpoint sets one.
- **`RotatingChannel::connect`.** Takes the TLS bound, and internal callers
  use `ConnectEndpoint::connect_rotating`, so that rotations are marked and
  correctly bounded.
- **Balanced channel and key-broker channel.** Left unmarked. The first has
  no connector hook; the second is not internal transport.
- **Marker `Display`.** Transparent, so status messages and any log or text
  consumers are unchanged.
- **Kind.** First skipped rather than attempted on a disk with 1.4 GB free. Rerun after the box owner authorized deleting `~/.kind-data`. No separate rebuild, because the Kind images use their own release build. The typed suite ran unsharded (`all`), covering every shard on one cluster.

## Results

| Item | Result |
|---|---|
| Marker, connector, classifier, caller routing | Done, pushed `8234955` |
| fmt / error policy / clippy (CI flags) / rustdoc | PASS |
| nextest grpc (no features; `internal-tls`) | PASS 78/78; 85/85 |
| nextest grpc + 70 dependents | 7787/7819. The 32 failures are identical on base: ENVIRONMENTAL / PRE-EXISTING |
| Mutations M1–M10 | All caught |
| 300× stress of new tests | 300/300 iterations passed (2400/2400 test runs, 0 failures, 310 s), on HEAD `8234955` |
| Kind disk prep (`~/.kind-data` removed, owner-authorized) | `/home/dev` 1.4 GB → 86 GB free |
| Kind baseline smoke (`smoke-kind-e2e.sh`) | PASS (exit 0) |
| Typed Kind suite, all scenarios | PASS: ECW 77/77, native 23/23, 0 inconclusive, exit 0 |
| Live log sweep | Marker observed on node-ctl's cluster-api channel (DNS, refused, timed out); no unexplained connect failures |
| Teardown | Cluster deleted, Tilt and port-forwards stopped |

**Verdict:** PASS. The change is complete, and validation steps 1 and 2 pass on this box. The only failures, 32 process-spawn tests, are identical on base and are ENVIRONMENTAL / PRE-EXISTING.
