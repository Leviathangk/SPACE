# Controller image build experiment

The original measurements below used the controller-only Cargo profile. The
current branch also supports the complete Kind artifact set; its bootstrap,
separate test targets, and whole-image measurements are documented in
[`../kind-bazel/README.md`](../kind-bazel/README.md). Recapturing a controller-only
profile switches the experimental generated catalog back to that narrower set.

Compare the existing controller Cargo + sccache + BuildKit build with
rules_rs + LLVM + rules_oci. This is an opt-in experiment, not a CI migration.
The original comparison retained production Cargo manifests, lockfile, Dockerfiles,
and CI configuration. The later reconciler extraction changes the Cargo workspace
and lockfile as documented below; production Dockerfiles and CI gates remain unchanged. Both paths produce a complete, loadable Linux/amd64 image archive.

The comparison fixes the source revision, Rust version, controller features,
release optimization, LTO setting, concurrency, runtime filesystem, image
configuration, and provenance. Cargo's compiler-artifact JSON is the independent
oracle for dependency features and the host/release compilation split. The
Bazel profile adapter is specific to that source revision and must be regenerated
when dependencies or controller features change. It is not a general replacement
for Cargo resolution.

## Boundaries

- Timed: dependency/repository setup, compilation, code generation, linking, image assembly, and Docker-format
  archive export. Image loading, publication, and cache export are outside timing.
- Prepared beforehand: runtime image, builder creation, and source snapshots.
  Toolchain assets are available locally from preparation. Cold means an empty
  compiler/action cache; dependency/repository setup remains inside timing.
  Cargo release `lto=false` and its default local ThinLTO behavior are retained.
- Both caches use local storage. This measures build/cache behavior, not S3 or
  remote-cache networking, CI queueing, Kind startup, tests, or Trunk validation.
- The old path uses its existing GCC/Wild native stack. The candidate uses LLVM.
  Results compare these complete stacks; they cannot attribute gains to Bazel alone.
- `provenance` changes only the image revision to a synthetic SHA. It measures
  revision-layer invalidation, not an actual private-repository Trunk merge.

## Preparation

Run from the repository root on an x86-64 Linux worker with Docker Buildx,
Python 3.11+, and Bazel 9.2.0. Use a task-owned BuildKit builder; no registry
credentials or production compiler-cache credentials are needed.

```sh
docker buildx create --name space-controller-benchmark --driver docker-container \
  --driver-opt image=moby/buildkit:v0.32.2@sha256:28a898719c18a33f4e8000685287fa36fd0dd9560c6440227d3a732d79bb41d8
python3 ci/benchmarks/controller-bazel/prepare_runtime.py --builder space-controller-benchmark
```

Capture the old build's release profile, then project it into Bazel. This first
Cargo build is preparation, not a reported timing:

```sh
python3 ci/benchmarks/controller-bazel/benchmark.py cargo --run-id profile --capture-profile
python3 ci/benchmarks/controller-bazel/profile.py
uv run ci/benchmarks/controller-bazel/resolver.py
python3 ci/benchmarks/controller-bazel/generate.py
```

The generator preserves Cargo target names and package environment values.
Snapshots default to `git archive HEAD` plus the benchmark scaffolding. Pass
`--worktree` to include uncommitted source changes in a separate experiment;
keep those results separate from the archived baseline.

The generated resolver workspace contains manifests and placeholder source files;
Bazel compiles the original source files. The scoped rules_rs patch makes the
captured feature profile authoritative, preserves host/release profiles, and
restricts the dependency closure. The SQLite patch only removes an embedded
sandbox path from the build script.

## Trials

Use a new run ID for each cold trial. Reference compilation deliberately disables
BuildKit's dependency/final compilation layer reuse and uses an empty task-specific
sccache mount. Bazel uses a fresh snapshot/output base and an empty task disk cache.
No shared caches are deleted.

```sh
python3 ci/benchmarks/controller-bazel/benchmark.py cargo --run-id trial1 --case reference --export-caches
python3 ci/benchmarks/controller-bazel/benchmark.py bazel --run-id trial1 --case reference
```

Run each pair serially. `warm` reuses the reference checkout; other cases create
new source snapshots. `unrelated` edits the API service outside the controller
closure; `controller` edits the controller entry point; `provenance` changes only
the revision label/file. All edits are confined to disposable snapshots. Edit markers and synthetic
revisions include the run ID, avoiding full-layer hits from a previous trial.
Use `--variant <unique-name>` when repeating a changed-source case within one
run; it creates another checkout and another marker.

```sh
python3 ci/benchmarks/controller-bazel/benchmark.py cargo --run-id trial1 --case warm
python3 ci/benchmarks/controller-bazel/benchmark.py bazel --run-id trial1 --case warm
```

Repeat with `unrelated`, `controller`, and `provenance`. `fresh` uses a new checkout.
For a truly fresh BuildKit worker, create another task-owned builder and supply
`--builder`; the harness imports the exported Rust and image layer caches. It primes the
compiler-cache mount before timing while keeping the measured seed bind empty,
so priming cannot invalidate the reference BuildKit layer keys.
A new checkout alone does not prove fresh-worker cache reuse.

Results, compiler-cache statistics, Bazel profiles/action logs, snapshots, and
archives are under `.cache/controller-benchmark/<run-id>/`. Inspect the return
code before interpreting any timing. Repeat trials to estimate variance.

## Image contract

Load the two reference archives under their benchmark tags, then run:

```sh
python3 ci/benchmarks/controller-bazel/validate.py
```

Validation compares image configuration, `--help`, `--version`, ELF format, and
all runtime filesystem content/metadata except compiled controller bytes and
Docker's ephemeral host files. Matching these is a prerequisite for timing claims;
it is not an application integration test.

Audit the executed compiler profile (including rustc response files):

```sh
python3 ci/benchmarks/controller-bazel/audit.py \
  .cache/controller-benchmark/trial1/bazel-reference/.cache/controller-benchmark/logs-reference/bazel-actions.json \
  --workspace .cache/controller-benchmark/trial1/bazel-reference \
  --output .cache/controller-benchmark/trial1/compiler-parity.json
```

## Gazelle-managed reconciler extraction

`crates/libs/cluster/controller/tombstone-retention` is the first flat Rust
package: `lib.rs`, an adjacent `retention.test.rs`, and its Cargo compatibility
manifest. Production and test sources are separate Bazel targets. Existing
Cargo-style packages retain their experimental manifest-generated targets.

Gazelle 1.2.1 runs only in packages opting in with `gazelle:lang rs`; the root
uses `generation_mode update_only`. Disposable `.cache` checkouts are excluded
from its index. External imports resolve through the central Cargo manifest and
lockfile; explicit internal mappings bridge the older `space_crate` targets.

```sh
bazel run //:gazelle -- -strict crates/libs/cluster/controller/tombstone-retention
bazel run //:gazelle -- -strict -mode=diff crates/libs/cluster/controller/tombstone-retention
bazel test //crates/libs/cluster/controller/tombstone-retention:retention \
  --test_strategy=standalone --test_env=SPACE_CI_REQUIRE_TEST_DEPS=1 \
  --test_env=DOCKER_HOST="$(docker context inspect --format '{{.Endpoints.docker.Host}}')"
```

The tests preserve the existing fake-sink and Postgres retention coverage.
They import the library's public interface; no production API was widened for
testing. The existing tracing target is pinned to
`pplx_space_cluster_controller_server::tombstone_retention`. SQL locator comments
now identify the extracted crate instead of the old controller module.

The resolver adds a test-only dependency catalog for Testcontainers. It does
not add those crates to the controller's release compilation closure or change
the Cargo-derived features of shared dependencies. The build remains a local
benchmark prototype, not a production CI migration.

To isolate a crate split from fresh-checkout setup, prepare the before/after
source trees with matching toolchains and run the same retention edit in each:

```sh
python3 ci/benchmarks/controller-bazel/split_benchmark.py \
  --backend bazel --before /path/to/unsplit/tree --after /path/to/split/tree \
  --disk-cache /path/to/task/cache --output /path/to/results
```

Use `--backend cargo --builder <task-builder>` for the Docker build path. The
runner warms each tree, changes the batch limit from 1,000 to 1,001, then builds
the complete image archive. It restores the original sources even after a failed
build. Logs and timings distinguish warmup from the measured edit. Run backends
serially on the same worker. These are warm same-checkout measurements, separate
from the earlier fresh-checkout matrix; they exclude a separate `bazel fetch`.

The measured extraction results are in [SPLIT_RESULTS.md](SPLIT_RESULTS.md).
Use a fresh `--batch-limit` for repeated production edits; `--edit-kind tests`
measures a test-file edit against the production image target.
