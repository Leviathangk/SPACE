# Node credential agent: wait up to 60 s for the authority

Branch `ac/credmgr-agent-startup-wait`, base `06ce7d2`, head **`b096c4a`**:

- `2169317` Let the node credential agent wait up to 60 s for its upstream authority
- `8d923b3` Name the underlying cause in the credential agent's still-waiting warning
- `b096c4a` Serve the credential agent's liveness while it waits for its authority (review round 1)

## Design (as of `b096c4a`)

- **Retry: the existing helper with a longer policy.** `AGENT_UPSTREAM_CONNECT_POLICY` is the standard
  connect policy with `max_elapsed = AGENT_UPSTREAM_CONNECT_WINDOW` (60 s) and no retry-count limit.
  Every per-attempt bound is unchanged (3 s name-resolution allowance, 3 s dial, 3 s TLS handshake,
  0.5 s margin; attempt cap 9.5 s with TLS). So is the 100 ms→1 s backoff with ±20 % jitter, and so
  are the per-retry debug log and metric. **The window bounds when the last attempt starts; the
  call ends within the window plus one attempt cap (≤ 69.5 s with TLS).** A failed attempt is
  redialed within about 1 s, so the agent connects about 1 s after the authority becomes reachable,
  or up to one attempt cap later if a dial was already in flight.
- **Why not an outer loop:** an explicit loop around the 10 s connect would log a misleading "exhausted" warning every
  round and count attempts per round. Instead `common-grpc` gained `connect_tcp_channel_with_retry_observed`
  (the same helper plus an observer of each redialed attempt), and the credmgr client gained
  `connect_with_retry(policy, on_retry)`. Every other caller's behaviour is unchanged.
- **Health (round 1, MAJOR).** `start_agent` binds the health socket *before* the wait, using
  `LocalHealthServer::spawn_optional_waiting`, as sbx-mgr does. While the agent waits, `liveness` answers SERVING and
  readiness (`""`/`readiness`) answers NOT_SERVING. After the connect, the order of steps is the same as before:
  volume-socket preflight, then `mark_takeover_ready` (passive readiness), then the service mutex, then activation. If the wait gives up, fails setup
  or is cancelled, the health server's child token is cancelled and the server is joined before
  returning. If activation fails after the connect, the health server is cancelled. The authority role
  still binds its health socket after initialization, as before. The `--health-socket` help text and the doc comments
  now describe this.
- **Kind chart probes** (`space-node-service.probes`): startupProbe `-service=liveness` (period 5 s,
  300 s budget), readinessProbe `-service=readiness`, livenessProbe `-service=liveness` (period 20 s ×
  3). The startup probe now passes as soon as the socket is bound, during the wait. It probes liveness, not
  readiness, so it cannot make the pod Ready. Readiness keeps the pod not Ready until the
  authority answers. No charts or probes changed.
- **Errors.** `AgentUpstreamConnect { window, source }`: "every attempt it started within 60s failed;
  the last: …". `AgentUpstreamSetup(source)` covers failures before any attempt is redialed (CA read,
  TLS config, endpoint check). The window is far longer than an attempt cap, so a failed attempt is
  always redialed at least once, and none being redialed means the failure came from setup.
- **Warnings.** At most one per 10 s of wait, carrying elapsed time, attempts so far, the window and the error chain with repeated layers removed.
  They are emitted on failed attempts, so the gaps are ≥ 10 s and up to 10 s plus one attempt and
  backoff; on Kind, the gaps were 10.1–12 s.
- **Shutdown during the wait** ends it cleanly. The wait plus one attempt cap can outlast a pod's
  termination grace period. *Correction to round 1:* the Kind credential-manager chart sets
  `terminationGracePeriodSeconds: 60`, not the 30 s default I reported. Code comments cite no value.
- **Test seam.** The wait runs through a private `AgentUpstream` trait (the endpoint in production), and
  the reports go through an injectable sink. `common-grpc`'s `test-support` module exposes the real
  retry loop (`test_support::retry_tcp_connect`), so stand-in attempts run under the agent's real
  policy and observer.

## Tests (`credmgr/server/src/run/tests.rs`)

Paused clock, with stand-in attempts through the real retry loop (deterministic, no kernel timing):

| Test | Proves |
|---|---|
| `agent_connects_to_an_authority_that_comes_up_late` | reachable at 30 s → connected by an attempt ≤ one standard max backoff (1.2 s) after; every gap between attempts ≤ 1.2 s |
| `agent_connects_one_attempt_cap_after_a_dial_in_flight` | stalled dials → connected ≤ cap + backoff after reachable |
| `agent_gives_up_on_an_unreachable_authority_after_the_window` | `AgentUpstreamConnect { window }`, message names the window, ends in (window − 1.2 s, window], gaps ≤ 1.2 s |
| `the_last_attempt_starts_within_the_window_and_may_run_to_its_cap` | last start < window; end = last start + cap |
| `shutdown_ends_the_agents_wait_for_its_authority` | cancel at 30 s → `Ok(None)` at exactly 30 s |
| `still_waiting_reports_are_spaced_by_the_interval` | through the sink: >1 report, ≤ window/interval, first ≥ 10 s, gaps ≥ 10 s, attempts increasing |
| `an_unreadable_ca_is_a_setup_error_not_a_window_of_attempts` | real TLS endpoint, missing CA → `AgentUpstreamSetup(TlsRead)` |

Real I/O, without tight timing bounds:

| Test | Proves |
|---|---|
| `agent_connects_to_a_real_authority_that_comes_up_late` | real endpoint and loopback socket listening at 30 s → connects; reports carry "…: Connection refused (os error 111)", no repeated layer |
| `agent_withholds_readiness_but_answers_liveness_while_it_waits` | real health socket: liveness SERVING, readiness and `""` NOT_SERVING during the wait; readiness SERVING after the authority answers |
| `an_agent_that_gives_up_stops_its_health_server` | after a failed startup, the health socket no longer answers |

`common-grpc`: `the_retry_observer_sees_each_redialed_attempt`. The first round's 1 ms waker tests were
replaced: it bounded virtual-time jumps, not kernel completion, so their timing claims were not
deterministic. No test pins a configuration value.

**Mutations (14/14 killed; each run against all 11 tests):**

| Mutation | Killed by |
|---|---|
| window back to 10 s | comes_up_late, dial_in_flight, real_authority, gives_up, shutdown, reports_spaced |
| retry count back to the standard 12 | comes_up_late, gives_up, shutdown, reports_spaced |
| backoff cap 5 s | comes_up_late, gives_up: **deterministic now** (gap assertion; 20/20 stress iterations fail) |
| rate-limit guard deleted | reports_spaced |
| observer replaced by a no-op in `connect_agent_upstream` | real_authority, gives_up, reports_spaced, last_attempt |
| endpoint impl drops the observer | real_authority |
| observer reports the wrong attempt count (common-grpc) | retry_observer |
| wait ignores shutdown | shutdown |
| error chain not deduplicated / top-level message only | real_authority |
| health bound after the connect again | withholds_readiness |
| readiness published before the connect | withholds_readiness |
| failed wait leaves health serving | gives_up_stops_health |
| setup failures reported as window failures | unreadable_ca, gives_up_stops_health |

*Corrections to round 1:*
- The 5 s-backoff mutation was only probabilistically caught by the connect test (3 of 5 runs). The
  give-up test also caught it in every observed run, but its band admits a lucky run, so that kill was
  not guaranteed either.
- The round-1 warning tests covered `WaitWarnings` alone, not its wiring. Both are fixed above.

**300 runs:** all 11 tests, `--stress-count 300`, passed 300/300 (385 s).

## Validation step 1

- fmt, `check-rust-error-policy.sh` and workspace clippy (`--workspace --locked --all-targets --all-features -D warnings`): clean.
  This needs `PROTOC_INCLUDE` on this box (ENVIRONMENTAL).
- nextest `rdeps(pplx-space-credmgr-client) | package(pplx-space-common-grpc)`: 5931 run, 5930
  passed, 31 `#[ignore]`d. The 1 failure is **PRE-EXISTING**:
  `net-gateway …interception_exemptions_relay_original_tls_only_after_admission` (3 s timeout).
- nextest `rdeps(pplx-space-common-grpc)` (the whole dependent graph): 7917 run, 7882 passed, 35 failed.
  **All 35 fail identically at base `06ce7d2`** (re-run by name: 0/35 passed):
  - 31 `spaced-guest-server` tests (`it`, `worker_subprocess`, unit) fail with `PermissionDenied` starting guest
    processes as this box's non-root user. ENVIRONMENTAL.
  - 1 `node-runtime-core` test, 1 `spaced-host-cli` test, and the net-gateway test above: PRE-EXISTING on this box.
  - None is change-induced. `pplx-space-common-svc` was not touched.

## Kind bring-up #3 (`b096c4a`, CI telemetry settings)

`DATA_PLANE_URL=http://127.0.0.1:8090 SPACE_KIND_OBSERVABILITY=off SPACE_KIND_TELEMETRY_MODE=datadog
bash scripts/tests/smoke-kind-e2e.sh` → **pass**. Authority Service created at 18:28:55Z; the agents' pods
started at 18:28:52, a 3 s gap.

| Pod | Pod start → Ready | Restarts | Connected after process start |
|---|---|---|---|
| credential-manager-6hzzh | 18:28:52 → 18:28:58 | 0 | 0.8 s |
| credential-manager-9tw68 | 18:28:52 → 18:29:04 | 0 | 4.9 s |
| builder-credential-manager-7mr7l | 18:28:54 → 18:29:05 | 0 | 4.9 s |
| builder-credential-manager-hfwcq | 18:28:54 → 18:29:00 | 0 | 0.7 s |
| credmgr-authority (×2) | 18:28:55 → 18:28:57 | 0 | — |

No "exhausted TCP gRPC connect retry budget" warning appeared, and no still-waiting warning either (every wait was under 10 s).
**Typed suite** (`SKIP_BOOTSTRAP=1`, admin token file and cold receipts as in CI, `all`, unsharded):
**ECW 77/77, native 23/23**. All credmgr pods still had 0 restarts afterwards.

**Forced outage on that cluster.** I scaled the authority to 0, deleted one agent pod, scaled the authority back to 2 after
47 s, and ran the chart's own probe commands in the new pod every ~2 s:

- Liveness: **SERVING in all 27 samples**, from 0.3 s after the container started through the end.
- Readiness: **NOT_SERVING throughout the outage**, SERVING from 4.7 s after the scale-up (the authority was Ready 1 s
  after it, and the agent connected 2.1 s after that).
- The pod went **Ready 6 s after the scale-up**; the readiness probe period is 5 s.
- **0 restarts.** The only probe event was the expected readiness NOT_SERVING. No liveness or startup
  probe failures.
- The agent waited 51 s and warned at 12.1/22.2/34.1/45.1 s (attempts 9/13/19/24). One attempt ended at its dial bound
  ("deadline has elapsed").
- A first outage attempt (C) used a probe script with an empty socket path, so its probe samples are void.
  Its pod facts agree: not Ready during the outage, Ready 4 s after the scale-up, 0 restarts.

## Earlier rounds (images from `2169317`/`8d923b3`; health still bound after the connect)

- **Bring-up #1** (CI telemetry settings): 0 s gap, 0 restarts, no "exhausted" warnings. Typed suite ECW 77/77, native 23/23.
  - 30 s outage: 0 restarts.
  - 75 s outage: the agent gave up at 58.9 s, after 51 attempts, with the window in the error; 1 restart, expected.
- **Bring-up #2** (developer default telemetry): measured gap 20–21 s (not the ~50 s expected), 0 restarts, no "exhausted"
  warnings. Two agents connected about 5 s after the authority because a dial was in flight.

## Housekeeping

- **Disk:** Docker root was below 20 GiB before each bring-up. Under the pre-authorization I listed and then
  removed only older `space-*:tilt-*` tags, keeping the newest image per repository (rounds: 26, 35, 13 tags). No other images, cache or
  volumes were touched. `/home/dev` stayed above 30 GB free.
- **Cleanup:** every cluster was torn down with `infra/space/kind/scripts/teardown.sh`. Tilt is stopped, there are no port-forwards or
  containers, and the watchers are stopped.
- **Review:** public-safety review of each staged diff found nothing; the gitleaks hook passed. No Co-Authored-By trailers.

## Results

| Check | Result |
|---|---|
| fmt / error policy / workspace clippy | pass |
| nextest credmgr dependents + common-grpc | 5930/5931; 1 PRE-EXISTING |
| nextest all `common-grpc` dependents | 7882/7917; all 35 failures identical at base (31 ENVIRONMENTAL, 4 PRE-EXISTING) |
| 11 agent tests × 300 | 300/300 |
| mutations | 14/14 killed (5 s backoff now deterministic) |
| Kind #3 (CI telemetry) smoke | pass; 0 restarts; no "exhausted" warnings |
| Kind #3 typed suite | ECW 77/77, native 23/23 |
| 47 s forced outage | liveness SERVING throughout, readiness NOT_SERVING until connect, Ready 6 s after the authority returned, 0 restarts |

**Verdict:** The round-1 MAJOR is fixed. A waiting agent now answers liveness and withholds readiness, so a
liveness probe can no longer end the 60 s window. On Kind it became Ready within a few seconds of the
authority returning, without restarting. The minors are addressed:
- the timing tests are deterministic, through the real retry loop;
- the warning wiring and health binding are covered and proven by mutation;
- the 5 s-backoff mutation is caught every time;
- the window, attempt cap and setup errors are worded accurately.

Remaining limit: the wait's real-world timing was exercised on Kind only up to the forced 51 s outage. No
bring-up reproduced the ~50 s developer-default gap.

## Review round 2 (`446137c`): dropped startup future

**Finding:** if the startup future is dropped or aborted while the agent waits (a timeout, an aborted
task, while the runtime keeps running), the explicit stop-and-join of the early health server never
ran. `LocalHealthServer` stops only through its token, so liveness kept answering SERVING for an agent
that was gone.

**Fix (only `run.rs`):** `start_agent` holds `health_shutdown.clone().drop_guard()` from the bind until
activation. On successful activation the guard is disarmed and the token handed over as before. The explicit
stop-and-join on every normal exit is unchanged. `LocalHealthServer` is untouched.

**Tests:**
- `dropping_a_waiting_agent_stops_its_health_server`: the agent waits on an authority that never
  answers; once liveness answers SERVING, the test aborts the task and requires liveness to stop
  answering within 2 s.
- `an_activated_agent_keeps_its_health_server`: liveness still answers after `start_agent` has returned
  an activation, which proves the disarm.
- Both passed 300/300 stress runs (406 s).

**Mutations** (all 29 `run::tests`):

| Mutation | Result |
|---|---|
| N1 drop guard removed | killed: dropping_a_waiting_agent |
| N2 guard not disarmed on activation | killed: an_activated_agent_keeps |
| M8 health bound after the connect (round 1) | killed: withholds_readiness, dropping_a_waiting_agent |
| M9 readiness published before the connect (round 1) | killed: withholds_readiness |
| M10 failed wait leaves health serving, guard also removed (round 1) | killed: gives_up_stops_health, dropping_a_waiting_agent |
| M10b failed wait skips the explicit stop, guard kept | **survives, by design**: the guard alone stops the server when `start_agent` returns; the explicit stop only adds the join before returning, which no test observes deterministically |
| M11 setup failures reported as window failures (round 1) | killed: unreadable_ca, gives_up_stops_health |

**Validation:**
- fmt, error policy and workspace clippy (CI flags): clean.
- nextest `rdeps(pplx-space-credmgr) | rdeps(pplx-space-credmgr-client)`: 5831/5832. The 1 failure is the
  PRE-EXISTING net-gateway TLS-interception test, which also fails at base.
- No Kind rerun (owner decision).

| Check | Result |
|---|---|
| fmt / error policy / clippy | pass |
| nextest credmgr crates + dependents | 5831/5832; 1 PRE-EXISTING |
| new tests × 300 | 300/300 |
| mutations | 6/7 killed; M10b survives by design (explained above) |

**Verdict:** the finding is fixed by construction. Dropping the startup future before activation now cancels the
early health server, and activation keeps it.
