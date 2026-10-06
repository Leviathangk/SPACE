# Dead-guest auto-suspend reproduction on Kind

**Result: hypothesis confirmed on Kind.**

The setup: a guest whose agent answers only in brief, rare windows, so the
pre-suspend filesystem flush succeeds on roughly 5% of attempts.

- **OLD (pre-budget retry loop):** suspended 6/6 stalled sandboxes, each after
  13–22 failed attempts (all more than 10), 39–67 min after the first attempt.
- **NEW (`main`, bounded budget):** in the same scenario, 5/6 used up the
  10-attempt budget in about 68–71 min. Each logged `auto-suspend retry budget
  exhausted; slow-retrying ~every 6h`. All five stayed `running`, composed health
  `Degraded`, and `space.sandbox.guest.health{lifecycle:started,health:degraded}`
  = 5 to the end of the window. One sandbox suspended on attempt 7.
- **Controls** (no stall) suspended on the first attempt on both builds, 0–1 s
  after the attempt started.

Two side findings change how the production signal should be read. See
[Side findings](#side-findings).

- Under OLD, a stalled sandbox sits in `lifecycle:pausing` about 97% of the time,
  so the `started`/`degraded` gauge undercounts it.
- On this harness, restarting the node-stack pod made already-running, healthy
  guests unreachable from the host. Those guests then follow the same
  flush-failure path.

## Public-safety note

The motivating evidence came from a private deployment. Per the repository's
public-reproduction rules, this report does not copy that deployment's names,
dates, counts or links. "The production pattern" below refers to that evidence
only in qualitative terms. "The budget change" means the change that introduced
`MAX_AUTO_SUSPEND_ERROR_STREAK`/`mark_suspend_error`. Everything below uses
synthetic sandboxes on a local Kind cluster.

## Environment

| | |
|---|---|
| Host | Linux 6.12 x86_64, KVM, 32 vCPU, 123 GiB, Docker 27.5.1 |
| Cluster | Kind v0.30.0 (`kind-space-local`), K8s 1.34, two sandbox workers, built with `infra/space/kind/scripts/bootstrap.sh` (no Tilt) |
| Template | `seed:latest` (locally built, published by the bootstrap job) |
| **NEW** | `main` @ `45c545cc`. node-stack image `sha256:823aab6d…`, `space-sbxmgr-svc` sha256 `690945c7…` |
| **OLD** | `d778c139` = `45c545cc` + the `REPRO ONLY` commit (below). node-stack image `sha256:5200cf22…`, `space-sbxmgr-svc` sha256 `40394333…` |
| Switching | `build-images.sh space-node-stack` + `kubectl rollout restart ds/space-node-stack`, with the binary hash checked in both pods. Rebuilding NEW afterwards reproduced the identical image ID `823aab6d…`. |

The **`REPRO ONLY`** commit (`d778c139`) changes two constants in
`crates/libs/sbx/mgr/server/src/auto_lifecycle.rs`:

- `MAX_AUTO_SUSPEND_ERROR_STREAK = u32::MAX`, so it never parks.
- `AUTO_SUSPEND_RETRY_BASE_SECS = 0`, so `next_auto_event_at = now` and the next
  5 s reconcile tick retries.

That is the "retry every reconcile tick forever" behaviour. The unit tests for
the budget are expected to fail on that commit, and it must not be merged. The
cluster was returned to the NEW build at the end.

## Scenario

All scripts are in `scripts/repro-dead-guest/` (standard library only; the
driver reuses `examples/first_sandbox.py`).

1. **Sandbox.** Native `POST /v1/sandboxes` with
   ```json
   {"wake":{"on":["access","command"]},
    "condition_sets":{"access_idle":{"kind":"inactivity","activity_source":"access"}},
    "stages":[{"condition_set":"access_idle","at":"90s","action":"suspend"}]}
   ```
   (`driver.py create`). Guest commands go through `POST /v1/sandboxes/{id}/commands`
   with `"user":"root"` (`driver.py exec`).
2. **Guest agent.** `spaced-svc`, run by systemd as `spaced.service`. Systemd is
   PID 1, so the agent can be stopped. The loop targets
   `systemctl show -p MainPID --value spaced.service` and re-resolves it on every
   cycle.
3. **Stall loop.** `driver.py stoploop` (the script is the `LOOP` constant in
   `driver.py`).
   - It is written to `/usr/local/bin/repro-stoploop` and started as root with
     `setsid nohup` from a command the agent runs, so it leaves the agent's
     process group.
   - It sleeps 5 s first, so the launching command exits and its data-plane
     stream closes before the first STOP.
   - It then loops: `kill -STOP <agent>`; sleep U[1200, 4800] s; `kill -CONT`;
     sleep 2 s.
   - Each STOP/CONT is logged with a UTC timestamp to
     `/var/log/repro-stoploop.log` and mirrored to `/dev/kmsg`.
4. **Runs.** `run-batch.sh <build> 6 1200 3600 2 90s` creates one control
   (no loop) and six stalled sandboxes.
   - After the launch nothing touches the sandboxes. Only management `GET`s, node
     logs and metrics are read. Commands and file operations count as activity
     and would reset both the idle clock and the error streak.
5. **Evidence collection.**
   - `collect-logs.sh` follows each worker's `/space/logs/sbxmgr.log`.
   - `statsd_sink.py` is a DogStatsD UDP listener run in each Kind node's network
     namespace (`docker run --network container:<node>`). It records
     `space.sandbox.guest.health`.
   - `analyze.py`/`batch-status.py` reduce the logs to attempts, reasons, park
     and suspend times, and health changes.
   - `cont-overlap.py` lines attempts up against the guest's CONT log.
   - `gauge-series.py` summarizes the gauge.
   - `model.py` is a Monte Carlo of the same scenario.
   - Derived evidence is in `scripts/repro-dead-guest/evidence/`.

### Calibration (why the STOP period is 20–80 min, not 20–60 s)

The first probe used the suggested loop (STOP 20–60 s, CONT 2 s). It did not
stall the suspend: the first auto-suspend attempt succeeded 23 s after it started
(`evidence/timelines/smoke-new-example.txt`).

The reason is that auto-lifecycle suspends get a **180 s** guest-preparation
budget (`GuestSnapshotPreparationBudget::AutoLifecycle`,
`crates/libs/sbx/mgr/server/src/guest_filesystem_flush.rs:37`). A flush sent to a
stopped agent is simply held until the next CONT and then succeeds. An attempt
therefore fails only if no CONT falls inside its 180 s window, so per-attempt
success ≈ (180 + 2)/(mean STOP + 2).

A mean STOP of 3000 s gives ≈6%, inside the requested 3–10%. Observed per-attempt
success inside the windows was 7/167 = 4.2% (NEW 1/57, OLD 6/110). The model
predicts 5.0%. The STOP distribution was fixed before either build ran, and both
builds ran the identical scenario.

A stopped agent produces three failure forms, all reported as `suspend_failed`
`operation=guest_filesystem_flush`, `reason=guest_fs_flush_failed`:

- **180 s timeout** ("Timeout expired" / "request deadline exceeded"): 157 of
  the 160 in-window failures (156 "Timeout expired", 1 "request deadline
  exceeded").
- **Fast "transport error"** (`Unavailable`, about 0.14 s): this appears after
  about 65–70 min of continuous STOP, once the guest's vsock accept backlog is
  full of unaccepted probe connections. It was seen on the 10th NEW attempt for 3
  sandboxes (the other 3 of the 160 failures).
- **Watchdog effect:** while stopped, the spaced-guest probe fails, either by
  timing out (500 ms) or with the same fast transport error. Composed health goes
  `Degraded` about 60 s after the first STOP and stays there.

## Results

### Per-run timeline

All times are UTC on 2026-10-06. "First attempt" is the first `suspend_requested`
(`trigger=auto_lifecycle`). Full per-event timelines are in
`evidence/timelines/`.

| Run | Sandbox | STOP length (guest log) | Attempts | Failure reasons | Park (budget exhausted) | Suspend (artifact published) | Time first→suspend | Health at end of window |
|---|---|---|---|---|---|---|---|---|
| NEW control | `…49b0…` | – | 1 | – | – | 08:01:55 | 0 s | Healthy → suspended |
| NEW stall-1 | `…49bd…` | not read | 10 | 10 × guest_fs_flush_failed (10 timeout) | **09:12:24** | never (window to 09:45) | – | `started`, Degraded |
| NEW stall-2 | `…49c1…` | 1870 s → CONT 08:31:37.56 | 7 | 6 × guest_fs_flush_failed | – | 08:31:38.13 | 29.7 min | suspended |
| NEW stall-3 | `…49d4…` | not read | 10 | 10 × (10 timeout) | **09:12:24** | never | – | `started`, Degraded |
| NEW stall-4 | `…4a04…` | not read | 10 | 10 × (9 timeout, 1 transport) | **09:09:24** | never | – | `started`, Degraded |
| NEW stall-5 | `…49e6…` | not read | 10 | 10 × (9 timeout, 1 transport) | **09:09:24** | never | – | `started`, Degraded |
| NEW stall-6 | `…49ba…` | not read | 10 | 10 × (8 timeout, 1 deadline, 1 transport) | **09:09:24** | never | – | `started`, Degraded |
| OLD control | `…c44d…f5a6` | – | 1 | – | – | 09:50:32 | 0 s | Healthy → suspended |
| OLD stall-1 | `…c43e…9800` | 2794 s → CONT 10:35:40.76 | 15 | 14 × (14 timeout) | never | 10:35:41.33 | 45.1 min | suspended |
| OLD stall-2 | `…c44d…0709` | (not readable, see below) | 22 | 21 × (21 timeout) | never | 10:57:41 | 67.2 min | suspended |
| OLD stall-3 | `…c44d…753b` | 2436 s → CONT 10:29:42.85 | 13 | 12 × (12 timeout) | never | 10:29:43.43 | 39.2 min | suspended |
| OLD stall-4 | `…c43e…cab6` | (not readable) | 21 | 20 × (20 timeout) | never | 10:53:36 | 63.1 min | suspended |
| OLD stall-5 | `…c441…` | (not readable) | 20 | 19 × (19 timeout) | never | 10:51:10 | 60.6 min | suspended |
| OLD stall-6 | `…c43f…` | (not readable) | 19 | 18 × (18 timeout) | never | 10:47:17 | 56.7 min | suspended |

Notes on the table:

- NEW first attempts were at 08:01:54–55; the window ran to 09:45 (about 103 min
  after the first attempt). OLD first attempts were at 09:50:32–33; the window
  ran until all six had suspended at 10:57.
- One 600 s-idle smoke sandbox (no stall) also suspended 0.5 s after its first
  attempt.
- **The NEW attempt ladder was exactly as coded.** Attempts started at +0, +190,
  +390, +610, +870, +1210, +1710, +2490, +3270 and +4050 s, each holding the
  180 s budget, followed by the park. After the park, no further attempt was
  logged for the rest of the window.
- **OLD retried 5 s after every failure**, e.g. `suspend_failed` 09:53:32.76 →
  `suspend_requested` 09:53:37.76.
- **Every successful stalled suspend landed inside the guest's 2 s CONT window.**
  - NEW stall-2: CONT 08:31:37.56 → published 08:31:38.13.
  - OLD stall-1: CONT 10:35:40.76 → 10:35:41.33.
  - OLD stall-3: CONT 10:29:42.85 → 10:29:43.43.
  - Source: `evidence/guest/*.cont-overlap.txt`.
  - The other STOP logs could not be read, because reading needs the agent and
    every read counts as activity (see the next section).

### Expected vs observed (`model.py`, 20 000 trials, same parameters)

| | Model | Observed |
|---|---|---|
| NEW: park probability | 0.72 | 5/6 |
| OLD: suspends within window | 1.00 (the first CONT is always caught, except about 2% that fall in a 5 s gap) | 6/6 |
| OLD: attempts to suspend | median 16, p10–p90 9–24 | 13–22 |
| OLD: P(> 10 attempts) | 0.80 | 6/6 |
| Per-attempt success | 0.050 | 0.042 |

In the model, OLD's suspend time is simply the time to the guest's first CONT.
NEW's budget (about 1800 s of flush coverage spread over about 4230 s) catches
that CONT only about 28% of the time. After the park, NEW makes one attempt per
6 h, so each later attempt succeeds with probability ≈5% (expected time to
suspend ≈ 5 days at 4 attempts/day).

### Guest-health gauge

`evidence/{new,old}-gauge-series.txt` sums
`space.sandbox.guest.health{lifecycle:*,health:*}` over both workers.

- **NEW:** after the parks (09:09–09:12), `lifecycle:started,health:degraded` was
  a flat 5 until the window closed, one per parked sandbox. Before the parks it
  flickered between 0 and 6, because a sandbox inside a 180 s attempt is
  reported as `lifecycle:pausing`.
- **OLD:** each stalled sandbox spent about 97% of its time until suspension
  inside an attempt, i.e. in `pausing` (computed from the attempt timeline), so it
  barely contributed to `started`/`degraded`. The OLD-window gauge is
  contaminated by the extra sandboxes described below, so per-sandbox attribution
  comes from the logs, not the gauge.

## Is the hypothesis confirmed?

**Yes, for the mechanism:**

- With an intermittently responsive guest, the unbounded loop suspends the
  sandbox after more than 10 attempts, within about an hour, because its
  back-to-back 180 s attempts cover nearly all wall time.
- The 10-attempt budget gives up after about 70 min in most such cases.
- It then leaves the sandbox `running`/`Degraded` with one attempt every 6 h.
- Each 6 h attempt that fails re-parks immediately. `mark_suspend_error` resets
  the streak only when `now - next_auto_event_at > 6h`, but a park retry fires
  at about `next_auto_event_at`. This is derived from the code; the window was
  shorter than 6 h, so no 6 h retry was observed.

This matches the shape of the production pattern: incidence unchanged, far fewer
flush failures logged, a long-lived degraded population growing after the
budget change, and most unrecovered sandboxes needing more than 10 attempts under
the old loop.

**Not established here:**

- whether production's degraded guests stall in the same way (a frozen agent,
  rather than a hung kernel or an exhausted guest);
- why some production clusters are unaffected (their delete-rather-than-suspend
  workload was not tested);
- how much of the production gauge step is the `pausing` → `started` accounting
  shift rather than new sandboxes. See below.

## Side findings

1. **The gauge is not comparable across the change.** Under the old loop, a
   sandbox failing the 180 s flush spends about 97% of its time in
   `lifecycle:pausing`. Under the budget, it sits in `lifecycle:started`. Part of
   the step in `{lifecycle:started,health:degraded}` at the rollout is therefore
   a change in accounting. To compare before and after, count degraded sandboxes
   across `started` + `pausing`, or count distinct sandboxes with `guest_fs_flush_failed`.

2. **Activity clears the streak, and so do failed API probes.**
   - Retrying `POST /commands` against a stopped agent (503) still counts as
     command/access activity.
   - That re-arms the idle stage and clears the streak (`clear_auto_error`,
     `auto_lifecycle.rs:731`): the parked NEW sandboxes started a fresh retry
     ladder once my log readers began polling them.
   - Any client that polls a parked sandbox therefore gives it a fresh budget.
     This partly hides the leak and partly explains slow recoveries.

3. **A node-stack pod restart left healthy running guests unreachable from the
   host.** This reproduced on 3/3 restarts, with both image-changing and
   same-image restarts. Not root-caused.
   - Healthy fresh-boot and snapshot-restored sandboxes (exec working, composed
     `Healthy`) were unreachable from the host after
     `kubectl rollout restart ds/space-node-stack`:
     - the watchdog's spaced-guest probe failed in about 35 µs ("transport
       error");
     - `POST /commands` returned 503, with
       `guest_process_start_failed: spaced endpoint attestation failed`;
     - composed health went `Degraded` and stayed there (more than 90 min for one
       sandbox).
   - The guest itself was fine. A raw `CONNECT 5002` on the jail's `v.sock`
     (through `/proc/<firecracker pid>/root/v.sock` from the Kind node) returned
     `OK` in 0.36 ms.
   - So the host-side attestation/connection path is what breaks. Entry points:
     `crates/libs/sbx/mgr/server/src/service/spaced_bridge.rs:412,430` and
     node-manager's `validate_sandbox_endpoint_proof`
     (`crates/libs/node/mgr/server/src/service/endpoint_attestation.rs:125`).
   - Such sandboxes fail the pre-suspend flush in the same way, and under the
     budget they park.
   - If this also happens in production rollouts, it would directly add a batch
     of dead guests at each node-stack rollout, consistent with a step at the
     rollout itself. Whether it is Kind-specific is open: the Kind
     `AGENTS.md` lists restart-safety mounts and per-jail vsock setup as
     harness-sensitive.
   - The three probe sandboxes (`repro-rst-fresh`, `repro-rst-restored`,
     `repro-rst2-fresh`) were left running in the local cluster as live evidence.
     Delete them with `DELETE /v1/sandboxes/{id}` once they are no longer needed.

4. **Smaller oddities (not investigated):**
   - The public API kept reporting 3 OLD sandboxes as `running` for at least
     25 min after their node-side suspends had completed (10:51–10:57). Three
     earlier suspends propagated within a minute.
   - One restored sandbox went `failed` after the restart in item 3.

## Recommended fix

The doc comment on the budget states its premise directly
(`auto_lifecycle.rs:654`): "*A failed auto-suspend can only recur when its cause
is a wedged capture volume*". This reproduction falsifies that premise:
`guest_fs_flush_failed` on a degraded guest recurs and is intermittently
recoverable.

**Minimal fix (recommended first): don't count guest-preparation failures
toward the wedge budget.**

- Tag the guest-flush failure the way the backup-fence failure is already tagged.
  The `return Err(Status::internal("guest snapshot preparation before suspend
  failed…"))` at `grpc_lifecycle.rs:3254` would carry a metadata key similar to
  `AUTO_LIFECYCLE_SUSPEND_BACKUP_FENCE_FAILED_METADATA` (`grpc_lifecycle.rs:1387`).
- In `handle_auto_lifecycle_error` (`reconcile.rs:263-282`), route that class to
  a retry that never parks:
  - keep the exponential backoff, but cap it low (for example 30–60 s);
  - do not increment `auto_error_streak`, so a later capture wedge still gets its
    10-attempt budget;
  - keep the capture-wedge path (anything else) on today's budget and park.
- **Trade-offs:**
  - With a 60 s cap, a stalled guest's flush coverage is about 180/240 = 75%
    (OLD ≈ 97%, NEW ≈ 43% then about 0). Model it with `model.py` before
    picking the cap.
  - Each attempt holds one of the 8 per-node auto-lifecycle slots
    (`AUTO_LIFECYCLE_RECONCILE_CONCURRENCY`, `reconcile.rs:14`) for up to 180 s.
    With roughly 10 or more stalled sandboxes on one node, the old loop starved
    other sandboxes' transitions, which is a plausible reason the budget was
    added.
  - So also give flush retries their own small concurrency lane, or a shorter
    flush budget on retries, so dead guests cannot block healthy suspends.
  - Log volume comes back (one WARN per attempt); rate-limit the WARN per sandbox
    and count it in metrics instead.

**Alternatives:**

- **Keep the fast loop but raise the cap** (for example 30–50 attempts at a 60 s
  cap, then park). This is simpler, since it needs no classification, but it only
  moves the cliff: the model's OLD distribution has p90 = 24 attempts. Capture
  wedges then retry for longer, too.
- **Bounded fallback** (best follow-up after the minimal fix). After N flush
  failures on a sandbox whose composed health is `Degraded`, either:
  - (a) suspend without the guest flush. The memory snapshot carries the guest's
    dirty page cache, so the snapshot is self-consistent for memory restore.
    The open question for the snapshot owners is any consumer that uses the
    rootfs without the memory image (cold/filesystem-only restore, forks). The
    stop path already treats its flush as best-effort
    (`flush_guest_root_filesystem_before_stop`, `guest_filesystem_flush.rs:88`).
  - (b) fail the sandbox so the repair loop restores it from its last durable
    snapshot. This frees resources deterministically but loses guest state since
    that snapshot, which is user-visible data loss and needs a policy decision.
- **Monitoring regardless of the fix:**
  - alert on `auto_lifecycle_retries_exhausted` (emitted at park,
    `reconcile.rs:303`);
  - make the degraded-guest dashboard sum `started` + `pausing`.

**Also recommended:** confirm or rule out side finding 3 on a non-Kind node
before attributing the whole population to the budget. If node-stack restarts
strand healthy guests in production, the fix above recovers them only once they
become reachable again. Recovering them needs a re-attestation after restart, or
the fallback above.

## Reproduce

Run from the repository root on a Linux/KVM host, after `docs/installation.md`
(bootstrap):

```bash
D=scripts/repro-dead-guest
mkdir -p ~/repro-logs/statsd ~/repro-logs/runs && cp $D/statsd_sink.py ~/repro-logs/statsd/
for n in space-local-worker space-local-worker2; do
  docker run -d --name dd-sink-$n --network container:$n -v ~/repro-logs/statsd:/out \
    python:3.12-alpine python -u /out/statsd_sink.py /out/$n.statsd
done
python3 $D/driver.py login
nohup $D/collect-logs.sh ~/repro-logs/run-new/sbxmgr >/dev/null 2>&1 &
$D/run-batch.sh new 6 1200 3600 2 90s          # wait ~100 min
$D/batch-status.py new ~/repro-logs/run-new/sbxmgr
# OLD: cherry-pick the REPRO ONLY commit, then
#   bash infra/space/kind/scripts/build-images.sh space-node-stack
#   kubectl --context kind-space-local -n space rollout restart ds/space-node-stack
# then restart collect-logs.sh and run-batch.sh old ... in the same way.
```

Pitfalls found along the way (already handled in the scripts):

- Delete or ignore earlier sandboxes before switching builds. Do not read their
  guest logs while another build's window is open, because reads count as
  activity.
- A stopped agent makes `POST /commands` fail (503) until its next CONT, so
  `driver.py stoplog` retries for up to `--wait` seconds.
- `pkill -f` patterns must not match the invoking shell's own command line.
