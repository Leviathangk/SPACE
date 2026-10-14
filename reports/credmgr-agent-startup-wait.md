# Node credential agent: wait up to 60 s for the authority

Branch `ac/credmgr-agent-startup-wait`, base `06ce7d2`, head `8d923b3` (2 commits):

- `2169317` Let the node credential agent wait up to 60 s for its upstream authority
- `8d923b3` Name the underlying cause in the credential agent's still-waiting warning

## Design

- **Retry mechanism: reused the existing connect helper with a longer-budget policy.** I did not write an outer loop.
  `AGENT_UPSTREAM_CONNECT_POLICY` is `DEFAULT_GRPC_CONNECT_RETRY_POLICY` with
  `max_elapsed = AGENT_UPSTREAM_CONNECT_WINDOW` (60 s) and `max_retries = usize::MAX`, so the
  window, not a retry count, ends the wait. Every per-attempt bound (3 s dial, 3 s lookup
  allowance + dial, 3 s TLS handshake, attempt cap) comes from the unchanged helper. The 100 ms→1 s
  backoff with ±20 % jitter and the per-retry debug log and `client_retry` metric are also unchanged.
- **Why not an explicit loop around the 10 s helper?** Each 10 s segment would log the helper's
  "exhausted TCP gRPC connect retry budget" warning while the agent was still waiting, which is
  misleading and noisy. It would also report per-segment attempts instead of the total.
- **Visibility.** `common-grpc` gains `connect_tcp_channel_with_retry_observed`. It is the same helper,
  but it hands an observer each failed attempt it is about to redial: `TcpConnectRetry { attempts,
  elapsed, error }`. The final failure is returned, not reported. `connect_tcp_channel_with_retry`
  and every other caller delegate to it with a no-op observer, so their behaviour is unchanged. The credmgr client gains
  `CredmgrEndpoint::connect_with_retry(policy, on_retry)`, and `connect()` delegates to it with
  the default policy. Only TCP/TLS endpoints report retries; the agent already rejects a UDS
  upstream.
- **Rate-limited warn.** `WaitWarnings` makes one warning due per 10 s of elapsed wait. The warning
  carries `elapsed_ms`, `attempts`, `window` and the attempt's error with its sources, deduplicated
  (commit 2; on Kind the bare Display was only "transport error").
- **Giving up.** `RunCredmgrError::AgentUpstreamConnect { window, source }` displays
  "… could not connect to the upstream authority within 60s: <cause>". The binary's anyhow exit
  prints the full cause chain. The helper still logs its one "exhausted" warning at that point.
- **Readiness unchanged.** The connect still finishes before the volume-socket preflight,
  the health socket and the service mutex: `run_agent` connects first, then calls
  `initialize_node_local` with the connected client. No chart or probe changes.
- **Shutdown during the wait (decision).** The DaemonSet uses the default 30 s termination grace
  period. Today's connect (≤ ~20 s) ends inside it; a 60 s wait would not. `connect_agent_upstream`
  therefore races the connect against the shutdown token. It returns `None` and `run_agent` exits cleanly
  ("terminated while waiting for the upstream authority"). Nothing else in the agent or authority paths changed.

## Tests (`credmgr/server/src/run/tests.rs`, `common/grpc/src/connect/tests.rs`)

All tests use the paused clock against a loopback port this test owns, bound but not listening, so dials are refused until
`listen()`. Tokio's paused clock jumps to the next timer whenever all tasks wait on I/O, and a
loopback dial's result is seen only after one such jump. Without help, every dial would cost up to its own 3 s
bound (measured: 13 of 17 refused attempts took about 3 s). A spawned 1 ms sleep loop
(`tick_paused_clock`) caps each jump at 1 ms. A 60 s window runs in about 0.2 s of wall time.

| Test | Proves |
|---|---|
| `agent_connects_to_an_authority_that_comes_up_late` | authority listens at 30 s; agent still waiting at 30 s, then connects within one standard-policy max backoff (`max_delay·(1+jitter)`) + 100 ms |
| `agent_gives_up_on_an_unreachable_authority_after_the_window` | `AgentUpstreamConnect { window == AGENT_UPSTREAM_CONNECT_WINDOW }`, message names the window, gives up in (window − max backoff, window + 100 ms] |
| `shutdown_ends_the_agents_wait_for_its_authority` | cancel at 30 s → `Ok(None)` within 100 ms |
| `still_waiting_warnings_are_spaced_by_the_interval` | `WaitWarnings` over a 4-interval wait: >1 warning, first ≥ interval, gaps ≥ interval |
| `still_waiting_warnings_name_the_cause_a_transport_error_wraps` | warn cause ends with the OS "Connection refused" text, several layers, no repeated adjacent layer |
| `the_retry_observer_sees_each_redialed_attempt` (common-grpc) | observer sees attempts 1..n−1 with their own errors, not the final one |

No assertion pins a configuration value. Bounds are expressed through the named constants, and the
"about 1 s" bound comes from the *standard* policy's backoff. The crate has no log-capture helper
(the observability `test-capture` feature is not a dependency here), so per the task the warn log itself is not
captured; its rate limiting is tested through `WaitWarnings`.

**Mutations** (each run with all six tests):

| Mutation | Killed by |
|---|---|
| M1 agent `max_elapsed` back to 10 s | connects_late, gives_up, shutdown |
| M1b agent uses `DEFAULT_GRPC_CONNECT_RETRY_POLICY` | connects_late, gives_up, shutdown |
| M2 agent `max_delay` 5 s | gives_up (every run), connects_late (3/5 runs) |
| M3 `WaitWarnings::due` always true | spaced_by_the_interval |
| M4 observer reports the wrong attempt count | the_retry_observer_sees_each_redialed_attempt |
| M5 wait ignores shutdown | shutdown |
| M6 no dedup in the error chain | name_the_cause |
| M7 warn shows only the top-level message | name_the_cause |

M2 initially survived, because the first version derived the bound from the agent's own policy. I re-derived it from the standard policy.
**300 runs:** `--stress-count 300` on the final six tests passed 300/300 (522 s). An earlier
300/300 run covered the first five.

## Validation step 1 (this box)

- `cargo fmt --all -- --check`: clean. `scripts/check-rust-error-policy.sh`: clean.
- `cargo clippy --workspace --locked --all-targets --all-features -- -D warnings`: clean, on the
  final code. Initially it failed ENVIRONMENTAL: the `protoc` symlink did not expose well-known
  includes; fixed by setting `PROTOC_INCLUDE=~/.local/protoc/include`.
- nextest `rdeps(pplx-space-credmgr-client) | package(pplx-space-common-grpc)`: 5926 run,
  **5925 passed, 1 failed, 31 ignored** (`#[ignore]`, mostly root-only node-mgr/volmgr; not run).
  - `pplx-space-net-gateway tcp_proxy::tests::tls_interception::interception_exemptions_relay_original_tls_only_after_admission`
    times out at 3 s ("exempt host: Elapsed"). **PRE-EXISTING**: it fails identically at base
    `06ce7d2` on this box (1/1, and 1/30 in a stress run), and it exercises the TCP proxy, not gRPC connects.
    I did not investigate the cause further.

## Validation step 2: Kind bring-up, CI telemetry settings

`DATA_PLANE_URL=http://127.0.0.1:8090 SPACE_KIND_OBSERVABILITY=off SPACE_KIND_TELEMETRY_MODE=datadog
bash scripts/tests/smoke-kind-e2e.sh` → **exit 0** (7/7 round-trip). Images built from `2169317`.
Authority Service `space/space-credmgr-authority` created **15:40:19Z**, the same second as the agents,
so there was **no gap** in this bring-up.

| Pod | Pod start | Ready | Start→Ready | Restarts |
|---|---|---|---|---|
| space-credential-manager-6zfz7 | 15:40:19 | 15:40:25 | 6 s | 0 |
| space-credential-manager-vhc64 | 15:40:19 | 15:40:29 | 10 s | 0 |
| space-builder-credential-manager-m8zpd | 15:40:20 | 15:40:26 | 6 s | 0 |
| space-builder-credential-manager-w7wkt | 15:40:20 | 15:40:26 | 6 s | 0 |
| space-credmgr-authority-…-mq48z / -mn98s | 15:40:19 | 15:40:20 | 1 s | 0 |

The agents connected about 4 s after process start (debug-level retries only, below the 10 s warn threshold).
"exhausted TCP gRPC connect retry budget": **none** in any bring-up pod log or in the smoke log.

**Typed suite** on that cluster: `SKIP_BOOTSTRAP=1`, with the admin token file and cold-receipts dir
as in CI, `smoke-kind-typed.sh all` over the full inventory (no shard index, because one cluster ran everything):
**ECW 77 passed / 0 failed; native 23 passed / 0 failed** (15:42–16:32Z). After the suite, all credmgr pods still had 0 restarts.

Because this bring-up never exercised the wait, I ran two demonstrations on the same disposable cluster.
Each scaled the authority to 0, deleted one agent pod, and scaled the authority back to 2 later:

- **Demo A, 30 s outage:** the new agent warned at 11.5 s (14 attempts), 22.0 s (22) and 32.2 s (31),
  then connected 2 s after the authority went Ready (16:33:51 → 16:33:53.6). Pod start→Ready 40 s,
  **0 restarts**.
- **Demo B, 75 s outage** (after Tilt redeployed the commit-2 code): first run warned at
  11.7/21.8/32.0/42.3/53.1 s (attempts 14/23/31/38/47) with cause "transport error: tcp connect
  error: Connection refused (os error 111)". It logged one "exhausted … retry budget" warning at
  58.9 s (51 attempts), then exited 1: "node credential agent could not connect to the upstream
  authority within 60s: …", followed by the full cause chain. The restarted run connected 2 s after the
  authority returned. **1 restart, as expected for an outage longer than the window.**
- Tilt's mid-run rebuild at 16:43 redeployed the authority and all four agents together: 0 restarts, start→Ready 5–7 s.

## Validation step 3: Kind bring-up, developer default telemetry

The same command with `SPACE_KIND_OBSERVABILITY` and `SPACE_KIND_TELEMETRY_MODE` unset → **exit 0**.
Images built from `8d923b3`. Authority Service created **17:06:06Z**. Agents started at
17:05:45–46, a **measured gap of 20–21 s** (not the ~50 s the owner expected).

| Pod | Pod start | Ready | Start→Ready | Restarts | Warnings (elapsed, attempts, cause) | Connected after authority Ready (17:06:07) |
|---|---|---|---|---|---|---|
| space-credential-manager-jv6gr | 17:05:45 | 17:06:10 | 25 s | 0 | 10.8 s/14 dns "No address…"; 21.0 s/24 refused | +2.0 s |
| space-credential-manager-wbll2 | 17:05:45 | 17:06:15 | 30 s | 0 | 10.9 s/14 dns; 24.2 s/24 tcp "deadline has elapsed" | +5.2 s |
| space-builder-credential-manager-8lbmv | 17:05:46 | 17:06:12 | 26 s | 0 | 10.2 s/13 dns | +1.5 s |
| space-builder-credential-manager-qkb4z | 17:05:46 | 17:06:17 | 31 s | 0 | 10.6 s/14 dns; 22.4 s/22 tcp "deadline has elapsed" | +5.3 s |
| space-credmgr-authority-…-s822p / -zlbw6 | 17:06:06 | 17:06:07 | 1 s | 0 | — | — |

**No agent restarted. No "exhausted TCP gRPC connect retry budget" warning appeared.** Two agents connected
about 5 s after the authority was Ready, rather than about 1 s. Their attempt in flight when the Service
appeared hung on its dial and was cut by the 3 s dial bound ("deadline has elapsed"); the redial
after it succeeded. That matches a SYN sent to the new ClusterIP before kube-proxy programmed it on that
node, the case #309's dial bound exists for. The backoff itself never exceeded about 1 s.

## Housekeeping and decisions

- Docker root was below 20 GiB before each bring-up (19 GiB, then 16 GiB). Under the pre-authorization I
  removed only older `space-*:tilt-*` tags, keeping the newest image per repository, after listing them (26 tags,
  then 35). The single `space-rust-chef-base`/`space-rust-build-kind` images were not superseded,
  so I kept them. I did not touch third-party, base or Kind node images, the build cache or volumes.
  `/home/dev` stayed above 30 GB free (130–276 GB).
- The `aws` CLI symlink was broken; I reinstalled the CLI with `infra/space/kind/scripts/install-prereqs.sh`, as CI does.
- Cleanup: both clusters were torn down with `infra/space/kind/scripts/teardown.sh`. Tilt is stopped, there are no
  port-forwards, the task's test containers are drained, and the pod watcher is stopped.
- Commits have no Co-Authored-By trailer, per the task. A public-safety review of each staged diff
  found nothing; the gitleaks hook passed.

## Results

| Check | Result |
|---|---|
| fmt / error policy | pass |
| workspace clippy (CI flags) | pass |
| nextest credmgr dependents + common-grpc | 5925/5926; 1 PRE-EXISTING (net-gateway TLS interception, fails at base) |
| new tests × 300 | 300/300 |
| mutations | 8/8 killed |
| Kind #1 (CI telemetry) smoke | pass; 0 restarts; gap 0 s; no "exhausted" warnings |
| Kind #1 typed suite | ECW 77/77, native 23/23 |
| Demo A (30 s outage) | 0 restarts, warnings every ~10 s, connected +2 s |
| Demo B (75 s outage) | gave up at 58.9 s with window-named error, 1 restart, recovered +2 s |
| Kind #2 (dev telemetry) smoke | pass; gap 20–21 s; 0 restarts; no "exhausted" warnings |

**Verdict:** The change works as intended. On the bring-up that had a real gap (20–21 s), every agent waited through it
without restarting, logged at most two rate-limited warnings, and became Ready. Demo B also showed an outage longer than the
window ending in a clean, window-named exit. Nothing failed because of this change. Limits: neither bring-up reproduced
the ~50 s developer-default gap, and the CI-settings bring-up had no gap at all, so the 60 s window was only
exercised at 20–35 s on real bring-ups. In two of the four bring-up agents, connect-after-reachable was
about 5 s rather than 1 s, because the in-flight dial ran into its #309 dial bound.
