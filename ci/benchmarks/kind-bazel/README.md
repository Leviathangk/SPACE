# Full Kind Bazel experiment

Build the complete shared Kind artifact image once, then let Bazel schedule
individual smoke scenarios against invocation-owned, reusable Kind stacks.
This is an opt-in experiment; existing CI workflows and merge gates are unchanged.

## Build and framework tests

Run from the repository root with Linux/amd64, Python 3.11+, uv, and Bazel 9.2.0:

```sh
python3 ci/benchmarks/kind-bazel/bootstrap.py --source . --native-jobs 16
bazel build //ci/benchmarks/kind-bazel/artifacts:image_tarball
bazel test //space_smoke:unit --test_strategy=standalone
bazel query 'tests(//ci/benchmarks/kind-bazel:functional)'
```

Bootstrap uses a checked-in, compact **independent Cargo compiler-artifact
profile**, not a Cargo compilation on every worker. Its fingerprint covers Cargo
manifests, the lockfile, Rust pin, and the Kind Docker build plan. Dependency or
feature-plan changes fail closed until the profile is recaptured. The projected
resolver is generated under `.cache/controller-benchmark`; first-party BUILD files
and the pinned probe repository are generated from the same inputs.

The image contains all 27 Rust binaries, the existing pinned gRPC probe, and a
checksum binding the supervisor compatibility file to its executable. The
supervisor's distinct feature profile is preserved. Transitions normalize
unchanged dependencies so that profile does not duplicate their compilation.
Each binary has a separately cached, gzip-compressed image layer.

The public reference here is Docker mode, not the existing persistent incremental
Cargo mode. That alternative may also avoid the workspace rebuilds.

The native jemalloc build receives the selected job count rather than the
build-script runner's default of one. This is nested native parallelism: Bazel's
action count is not a strict cap on all compiler subprocesses. Match the worker
CPU allocation when choosing it.

The LLVM patch materializes its compiler headers before merging resource trees;
otherwise jemalloc's configure probe cannot find standard C headers. The existing
controller benchmark retains its prior flags and Cargo profile capture tooling.

## Functional targets and reusable workers

There is one target per eligible **scenario/API-surface pair**, generated from
the real registry. The current inventory has 101 targets. Existing quarantines
and ignored contracts remain visible in `targets.bzl`; they are not silently
enabled or counted as passes. There are also 43 separate targets for the existing
Python framework test files.

```sh
python3 ci/benchmarks/kind-bazel/inventory.py \
  --source . --output ci/benchmarks/kind-bazel/targets.bzl --check
```

Functional execution needs Linux KVM and a Docker daemon that permits privileged
containers, host cgroup namespaces, and native Docker volumes. Prepare the tools
image and a fixture bound to the freshly built artifact image:

```sh
docker build -f ci/benchmarks/kind-bazel/runner.Dockerfile \
  -t space-bazel-kind-runner:local .
python3 ci/benchmarks/kind-bazel/fixture.py \
  --source . --output /tmp/space-kind-fixture
```

`fixture.py` reports the artifact archive path. Pass that path below:

```sh
python3 ci/benchmarks/kind-bazel/pool.py run \
  --workers 1 \
  --source-archive /tmp/space-kind-fixture/source.tar \
  --artifact-archive /absolute/path/to/artifact/tarball.tar \
  --output /tmp/space-kind-results \
  //ci/benchmarks/kind-bazel:functional
```

Select individual labels instead of `:functional` to narrow a run, for example
`//ci/benchmarks/kind-bazel:ecw__ecw__command_round_trip`.

Bazel schedules targets; there is no duration table in this path. Each worker
owns a separate privileged container, network namespace, local Docker daemon,
Kind cluster, workspace, and native storage volumes. It bootstraps and runs the
existing baseline once, then leases the stack exclusively to one scenario at a
time. The first lease includes cold fixture setup, so the Bazel timeout allows
for that bootstrap as well as the scenario. Successful scenarios reuse it. A failed or abandoned lease invalidates
the worker before reuse. An invocation's cleanup removes only its own labeled
containers and their anonymous volumes.

The source fixture has its own synthetic Git commit; it does not claim to be an
upstream commit. A sidecar manifest binds the source/archive digests. No host
credentials or Docker socket are forwarded into the workers. Functional results
are never cached: the live kernel and cluster are external prerequisites. These
targets are manual and local; remote execution and cross-machine scheduling are
not configured.

Increase `--workers` only within the host's memory/storage capacity. More targets
do not make an individual recovery deadline or backoff interval shorter. Full
coverage still requires every target, including the encrypted restore scenario's
device-mapper prerequisites.

## Compare warm scheduling against fixed shards

`scheduling.py` prepares fresh isolated stacks for each arm, then runs the same
scenario/API-surface inventory with equal worker counts. The fixed arm uses the
existing duration-balanced shard assignments and sequential per-scenario
commands. The candidate uses actual Bazel test scheduling. Both use the same
fixture adapter, source archive, and artifact image, and run serially on one host.
This isolates scheduling; it does not exactly reproduce the old bulk-suite Python
process or a four-runner CI topology.

```sh
uv --no-config run --frozen --python 3.11 python ci/benchmarks/kind-bazel/scheduling.py \
  --workers 2 --source-archive /tmp/space-kind-fixture/source.tar \
  --artifact-archive /absolute/path/to/artifact/tarball.tar \
  --output /tmp/space-kind-scheduling-trial
```

Use `--plan-only` to inspect coverage without creating containers. Any deliberate
subset must use `--exclude SCENARIO_ID --exclusion-reason REASON`; exclusions are
recorded and never count as passes. Full coverage requires a host with all
scenario prerequisites, including device-mapper crypt/snapshot targets.

The report separates bootstrap, warm target execution, and cleanup. Candidate
execution includes Bazel startup/analysis overhead. A failed or incomplete arm
stops the comparison without claiming a speedup. Each arm gets new stacks and
native volumes; it does not inherit the other arm's live test state. Repeat with
`--dynamic-first` and a new output directory to examine order/cache effects.
A single paired trial is exploratory, not a variance estimate.

## Run on a self-hosted agent

Use a Linux amd64 worker with writable KVM, privileged Docker, and enough Docker
storage for the whole pool. Two stacks exhausted a roughly 158 GiB hosted disk
in an earlier incomplete run; a 500 GiB Docker disk provides more room without
lowering the suite's storage admission thresholds. Verify free space from a
Docker volume, since the checkout and Docker data may use different disks.

Keep the agent on an isolated queue and invoke this experiment manually. Preserve
Bazel outputs and downloaded image layers between jobs. The pool still creates
fresh live stacks for each invocation; it does not implement cluster persistence
across jobs.

When Docker runs in a sidecar, inspect the ancestor cgroup limits rather than
relying on `docker info` CPU counts. Pass `--cgroup-parent /absolute/host/cgroup`
to `scheduling.py` or `pool.py run` to place all workers below the assigned
allocation. The supplied parent must delegate memory and CPU controllers; a
process-bearing coordinator cgroup may not provide both. This mode moves the
worker init processes into a leaf and enables controllers before starting Docker.
It also uses private cgroup namespaces so nested Docker cannot
place ordinary child containers outside that hierarchy. Verify the resulting
containers' parent cgroup and namespace mode before accepting timings.

## Reproduce the whole-image comparison

Capture a new reference in a disposable source snapshot. This builds Cargo once
to obtain its independent profile and prepares the reference Dockerfile:

```sh
python3 ci/benchmarks/kind-bazel/prepare.py \
  --source . --destination /tmp/space-kind-reference \
  --builder space-controller-benchmark
```

Use a task-owned BuildKit builder as described in
`../controller-bazel/README.md`. Do not clear shared caches. When refreshing the
checked-in profile, review the snapshot's `cargo-profile.json`, copy it back, and
rerun bootstrap.

```sh
python3 ci/benchmarks/kind-bazel/compare.py \
  --source /tmp/space-kind-reference --output /tmp/space-kind-comparison \
  --builder space-controller-benchmark --marker unique-trial-id \
  --cases unchanged test-only controller controller-cli
python3 ci/benchmarks/kind-bazel/audit_graph.py \
  --source /tmp/space-kind-reference --output /tmp/space-kind-audit
```

Each backend warms first; measured builds run serially. Both produce complete
Docker-format artifact archives with local caches and 16 logical jobs. The
reference uses the public Kind Docker path, which does not enable sccache.
`controller` changes a source comment; `controller-cli` changes CLI help text and
therefore binary/image bytes. All source mutations are restored in `finally`.

Validate the measured Cargo archive against the candidate archive with
`validate.py CARGO_TAR BAZEL_TAR --output /tmp/artifact-parity.json`. It checks
inventory, ownership/modes, image configuration, ELF architecture/type, runtime
glibc requirements, probe bytes, and the supervisor checksum. The graph audit
follows artifact inputs from the final image, avoiding unused analyzed targets,
and compares cached as well as newly executed compiler actions with Cargo.

Results are in [RESULTS.md](RESULTS.md). Local incremental wins require retaining
the corresponding action outputs across CI workers; a production remote cache
or equivalent persistent cache is not configured by this experiment.
