# Kind: start the seed rootfs build as soon as the Rust artifacts exist

Branch `ac/kind-early-image-builds` (HEAD `eb68ccfc`), base `main` at `7a41941f`.
Status: **PARTIAL. Blocked on Docker-root disk space** (see "Blocker"). The change is
implemented and pushed. The bench covers S2 (2 runs per arm) and S1 arm A (1 run).
S1 arm B and correctness checks 1–4 need bring-ups the disk floor forbids.

## The change

1. `Tiltfile`: new `seed-artifacts` local_resource. `cmd` = `build-rust-artifacts.sh
   && build-seed.sh`, `deps` = the seed custom_build's deps (shared `SEED_DEPS`),
   `allow_parallel = True`, `resource_deps = ["rust-artifacts"]`, label `build`.
   `space-seed-publish`'s k8s_resource and its `resource_deps` are unchanged, so deploy
   ordering is unchanged.
2. The `space-seed-publish:local` custom_build still runs the full chain (pull bases →
   Rust artifacts script → build-seed.sh → docker build). The docker build now runs as
   `build-seed.sh -- docker build …`, i.e. under build-seed.sh's lock.
3. `build-seed.sh`: flock-based lock on `$OUT_DIR/.build.lock`, taken before the guest
   binary extraction, like build-rust-artifacts.sh: flock when present, otherwise log
   and run unguarded. The later caller waits, then finds the stamp current. The new
   optional `-- CMD …` execs CMD once the artifacts are current, still holding the lock.
4. Tiltfile comments explain why and cite the bench (S1-A1 below).

No worker local_resource (see decision D1).

## Decisions and reasons

- **D1: no early worker build.** The task assumed the worker build waits for space-api.
  That predates #154. Measured on this base, the node-service DaemonSets'
  `resource_deps` are bootstrap resources only, so the first of them
  (`space-node-manager`) starts the `space-worker:local` build together with the Rust
  build. Every run shows it: its build starts within 0.2 s of `rust-artifacts`. The
  other worker-image consumers (space-internal-api, credmgr-authority, the other node
  DaemonSets) show Tilt's `building-component space-worker:local` hold, not
  `waiting-for-dep`. An early worker local_resource would start at the same moment as
  the build it would warm, so it can't gain anything. The concurrent identical
  `docker build` question doesn't arise without it, so I didn't test it.
- **D2: packaging under the seed lock.** With an early resource, a later edit can start
  a rootfs rebuild (which first deletes `rootfs.ext4`/`manifest.json`) while the
  custom_build's `docker build` is reading the build context. Running the docker build
  as the `--` command of build-seed.sh closes that window. It is a small interface
  addition instead of a second lock protocol in the Tiltfile.
- **D3: the early resource also runs `build-rust-artifacts.sh` first.** `resource_deps`
  only gate the first run. After an edit, both resources fire together. Running the
  idempotent Rust script first (it waits on its lock, or finds the build current) makes
  build-seed.sh read the new artifact image instead of rebuilding the rootfs from the
  old one.
- **D4: bench env.** Same as the CI shard's telemetry env (`SPACE_KIND_OBSERVABILITY=off`,
  `SPACE_KIND_TELEMETRY_MODE=datadog`), with the local incremental Rust path (no
  prebuilt, no GHCR mirror). That matches the shard where the chain was measured and
  keeps public-registry pulls of the observability stack out of the timings.
- **D5: S1 design.** Re-applying a reverted edit hits every cache: synced Rust sources,
  seed stamp, BuildKit layers. So before every S1 run, outside the timed window, the
  runner rebuilds the Rust artifacts and seed for unmodified code; then it applies a
  one-line string edit in `pplx-space-spaced-common` (linked by spaced/spaced-svc and
  by worker binaries such as sbx-mgr, net-gateway, spaced-host and build-mgr). Cargo
  and the seed rootfs genuinely rebuild every S1 run. One fixed string is used for all
  S1 runs to limit Docker-root growth (identical binaries dedupe). Order: S2 A,B,A,B,
  then S1 A,B,A,B.
- **D6: no image deletion.** At the floor I stopped and reported, as instructed, rather
  than remove even task-created images.

## Blocker: Docker-root free space

The Docker data root on this box is a 50 GB filesystem. Readings (KiB, `docker run --rm
--entrypoint df <local image> -Pk /`):

| point | free KiB | GiB |
|---|---|---|
| box at start (no SPACE images) | 50,704,716 | 48.4 |
| after the cold warm-up and teardown | 30,243,220 | 28.8 |
| after S2 ×4 | 29,755,980 | 28.4 |
| after S1-A1 | 26,455,204 | 25.2 |
| before S1-B1, after its untimed prewarm | 25,070,408 | **23.9 < 25: stopped** |

An S1 run adds about 3.3 GB: a new artifact image, new worker/api layers and a new
seed-publish image. The seed rootfs is not byte-reproducible (rebuilding unmodified
inputs gave a new `content_hash`), so every rootfs rebuild adds a ~0.5–0.9 GB
seed-publish image. To resume: free space on the Docker host (operator decision) or use
a box with a larger Docker root, then rerun the remaining S1 runs and the correctness
checks with the runner described below.

## Verification done without a bring-up

- **L1, lock under a real rebuild:** `SPACE_SEED_REBUILD=1 build-seed.sh` plus a
  concurrent plain `build-seed.sh`. The second logged "another seed build holds …;
  waiting", "lock acquired after 21s", then "seed artifacts up to date … skipping
  build". The rootfs was built once.
- **L2, the `--` command holds the lock:** `build-seed.sh -- sh -c 'sha256 rootfs;
  sleep 15; sha256 rootfs'` plus a concurrent forced rebuild. The command saw the same
  hash at start and end, and the rebuild logged "lock acquired after 15s", then rebuilt
  (new hash afterwards). The exec'd command holds the lock fd (checked via
  `/proc/<pid>/fd/9`). Usage errors (`-x`, a bare `--`) exit with the usage line.
- `tilt alpha tiltfile-result` on B: evaluates; `seed-artifacts` has the expected
  cmd/deps/`AllowParallel`/`ResourceDependencies`.
- Existing shell tests that reference the Tiltfile:
  `scripts/tests/test-kind-port-forward-parity.sh` ok,
  `scripts/tests/test-kind-stale-tilt-recovery.sh` ok. No `test-kind-*.sh` sources or
  invokes the Kind build-seed.sh. `test-seed-build-metadata.sh` covers a different
  script, in build-mgr.
- In the B bring-ups, `seed-artifacts` ran right after `rust-artifacts` (2.2 s in S2,
  stamp current), and `space-seed-publish` then found the stamp current and only
  packaged (9–11 s, same as A).

## Other measurements (critical path, outside this change)

- **space-internal-api start = worker image done.** `provision-local-console` (a serial
  local_resource, needed by space-internal-api) is held while any build runs. In S1-A1
  it started at 257.2 s, the moment the worker build (started with Rust) finished. So
  in a changed-code bring-up the worker build is on the space-internal-api path, but it
  is not deploy-gated.
- **Interaction to watch:** `seed-artifacts` holds that serial resource the same way
  while it runs. When its rootfs build outlasts the worker build (e.g. a first run that
  downloads the Ubuntu squashfs), space-internal-api and the data plane become ready up
  to that difference later. The seed Job still completes no later than on A, because
  on A the same rootfs build runs after space-internal-api, in series.
- **space-object-store-bucket-init** took 66 s in S2-A1 and 6–7 s in the other runs. In
  S2-A1 it, not space-internal-api, gated the seed-publish build (start 112 s). Cause
  not determined; the cluster was gone before inspection.
- **credential-manager tail:** in every run the node credential-manager pods exited 1
  once or twice before the authority was reachable. In S2-A2, S2-B1 and S2-B2 (A and B
  alike) the second exit came about 2.5 min after the first, and they became Ready at
  226–234 s versus 100 s in S2-A1. That tail set the S2 wall time. It is unrelated to the seed path;
  treat S2 wall-time differences as this variance.
- UI, data-plane, cluster-controller and key-broker were never last: data-plane healthz
  came 25–67 s before the seed Job completed in every run. Left alone.

## Bench method

Box: 32 vCPU, 123 GiB RAM, Docker 29.8 (containerd image store), Kind via
`install-prereqs.sh`, KVM and dm targets as in kind-ci.yml. One untimed cold warm-up
from A (633 s, cold cargo build 363 s). Each run: `teardown.sh` → (S1/S2) untimed
prewarm of unmodified Rust artifacts + seed → (S1) edit → disk check → `bash
scripts/tests/smoke-kind-e2e.sh` with `DATA_PLANE_URL=http://127.0.0.1:8090`, timed,
stamped, `kind-bringup-timeline.py watch` + `snapshot` → teardown → revert. Times are
seconds after the smoke started. "Job applied/done" are the bootstrap log lines
"waiting … for the Job to complete" and "waiting for the worker and builder
node-service DaemonSets".

### Representative per-resource timelines (Tilt buildHistory start–finish, s)

| resource | S1-A1 (A, changed code) | S2-A2 (A) | S2-B2 (B) |
|---|---|---|---|
| rust-artifacts | 38.1–230.8 | 36.5–37.1 | 32.6–33.3 |
| seed-artifacts | — | — | 33.3–35.5 |
| space-node-manager (builds space-worker) | 38.3–257.2 | 36.6–48.9 | 32.8–45.3 |
| space-api | 53.4–240.8 | 51.9–58.1 | 47.7–54.1 |
| provision-local-console | 257.2–258.2 | 69.9–70.6 | 65.7–66.4 |
| space-internal-api | 258.2 | 70.6 | 66.4 |
| space-object-store-bucket-init Job | 42.7–49.7 | 41.9–47.9 | 38.1–45.1 |
| space-data-plane | 265.1–268.8 | 79.0–83.0 | 74.8–78.3 |
| space-seed-publish (build) | 265.1–311.3 | 79.0–88.1 | 74.8–84.3 |
| seed Job applied → done | 311.5 → 337.1 | 89.4 → 110.0 | 85.8 → 105.8 |
| credential-manager Ready | 270.7 | 225.9 | 232.1 |
| wall | 349.1 | 237.9 | 242.8 |

In S1-A1, the seed-publish build began 34.3 s after the Rust build finished. Its 46 s
build was about 22 s of rootfs (seed builder markers 267–289 s), then packaging and the
kind load. **Not measured:** B would move those ~22 s off the critical path, so the
S1-A1 shape predicts a seed Job about 20–25 s earlier. That projection is unverified
until S1-B runs.

## Results

### Per-run and per-arm medians (s; medians of 2 runs are means)

| run | arm | rc | wall | api healthz | seed Job applied | seed Job done |
|---|---|---|---|---|---|---|
| S2-A1 | A | 0 | 156.7 | 70.2 | 123.9 | 144.7 |
| S2-B1 | B | 0 | 248.3 | 63.5 | 86.9 | 108.1 |
| S2-A2 | A | 0 | 237.9 | 64.9 | 89.4 | 110.0 |
| S2-B2 | B | 0 | 242.8 | 60.2 | 85.8 | 105.8 |
| S1-A1 | A | 0 | 349.1 | 246.2 | 311.5 | 337.1 |
| S1-B1 | B | — | blocked (disk) | | | |
| S1-A2, S1-B2 | | — | not run | | | |

| scenario | arm | n | median wall | median healthz | median Job applied | median Job done |
|---|---|---|---|---|---|---|
| S2 | A | 2 | 197.3 | 67.6 | 106.7 | 127.4 |
| S2 | B | 2 | 245.6 | 61.9 | 86.4 | 107.0 |
| S1 | A | 1 | 349.1 | 246.2 | 311.5 | 337.1 |
| S1 | B | 0 | — | — | — | — |

S2 reading: the seed path in B is not slower. Excluding S2-A1's 66 s bucket-init
outlier, the Job was done at 110.0 (A) vs 105.8–108.1 (B). The wall-time medians are
set by the credential-manager tail (above), which hit A2, B1 and B2 and missed A1.

### Correctness checks on B

| check | result |
|---|---|
| 1. spaced edit → one rootfs rebuild, new content_hash packaged and deployed | **NOT RUN.** Needs a bring-up; blocked by disk. Lock behaviour covered by L1/L2 only. |
| 2. worker-only edit → worker image rebuilds, node DaemonSets roll | **NOT RUN.** Blocked. The change does not touch the worker build. |
| 3. early resource fails → custom_build still builds alone | **NOT RUN** in Tilt. Blocked. The custom_build's command is the same full chain as before, plus `--`. |
| 4. `smoke-kind-typed.sh all` with the CI shard env | **NOT RUN.** Blocked. |

### Verdict

Partial. The change does what it should where measured: on B the seed rootfs build
runs immediately after the Rust artifacts, concurrent callers serialize correctly, and
packaging can no longer race a rebuild. S2 shows no regression on the seed path.
The S1 benefit (projected ~20–25 s off the seed Job in a changed-code bring-up, from
S1-A1's 34 s gap and 22 s rootfs) and correctness checks 1–4 are **unverified**,
because the Docker data root fell under the 25 GiB floor (23.9 GiB) before S1-B1.
The worker half of the task needs no change on this base (D1).
