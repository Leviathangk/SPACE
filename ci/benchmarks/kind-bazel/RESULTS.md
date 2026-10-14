# Complete Kind artifact image: incremental comparison

The complete producer benefits from retaining unchanged workspace outputs. A
controller CLI change that modifies binary/image bytes took **236.7 seconds with
the public Cargo Docker build versus 39.9 seconds with Bazel**: 83.2% less time,
or about 3 minutes 17 seconds saved in this sample.

| Warm local-cache case | Cargo + BuildKit | Bazel + LLVM + OCI |
| --- | ---: | ---: |
| Unchanged | 1.5s | 0.6s |
| Comment in an existing controller test module | 229.4s | 0.5s |
| Comment in controller entry point | 237.4s | 32.6s |
| Controller CLI help text, changing binary bytes | 236.7s | 39.9s |

The source-comment cases measure invalidation; they need not change executable
bytes. The CLI case does. Bazel executes no actions for the test-only case, one
Rust action for the entry-point comment, and one Rust action plus one binary
layer and image/archive assembly for the CLI case. Other binary layers are reused.

## Boundary and interpretation

Both backends build the complete Linux/amd64 Docker-format artifact archive:
27 Rust executables, the same pinned gRPC probe, and the supervisor compatibility
checksum. Source, Cargo features, Rust 1.98.0, release optimization, disabled LTO,
and 16 logical jobs match. Builds run serially on one worker after warming each
backend's caches. Cargo uses the public Kind Docker path; sccache is disabled as
in that path. The baseline uses GCC/Wild and the candidate LLVM.

The timer includes incremental compilation, linking, image assembly and archive
export. It excludes builder/tool provisioning, source snapshot creation, image
loading, publication, cluster startup and test execution. These are individual
trials, not statistical estimates or measurements of a complete hosted pipeline.
The subsecond unchanged-build difference is not a meaningful capacity claim.

The Cargo Dockerfile copies the Rust workspace into a build stage based on cooked
dependencies. Changing a file under that COPY invalidates the stage; the prior
real workspace compilation outputs are not carried forward. The logs show the
workspace binaries rebuilding even after a cfg(test)-only comment. Bazel reuses
unchanged crate outputs and excludes test-only sources from production actions.

This differs from the older controller-only experiment: that narrower image had
much less unrelated workspace work to avoid. It also differs from the existing
**persistent incremental Cargo mode**, which this table does not benchmark.
Persisting Cargo's workspace target directory could recover similar reuse; this
experiment does not establish Bazel as faster than that alternative.

Fresh-worker or cold-build improvement is **not established**. Production CI
would need an appropriate persistent/remote action cache and an equivalent
fresh-worker comparison before assuming these local incremental timings apply.
The native jemalloc job setting allows nested parallelism; logical Bazel jobs
are not a hard cap on all compiler subprocesses.

## Validation

- The artifact-input graph contains all **810 distinct Cargo package/profile
  combinations**, including the separate supervisor feature profile. The audit
  reports zero missing units and zero feature/optimization differences. It
  includes cached compiler actions and excludes unused analyzed targets.
- Artifact validation passes for all 29 files: inventory, modes/ownership, image
  configuration, ELF architecture/type, required glibc versions, exact probe
  bytes and the supervisor checksum. Rust executable bytes differ between native
  toolchain stacks; this is not a proof of identical application behavior.
- All 27 Rust binaries also resolve their shared libraries under the existing
  Debian bookworm/glibc 2.36 runtime, checked with its ELF loader without
  starting application services.
- All **43 existing Python framework test-file targets pass** under Bazel.
- The generated **101 functional scenario/API-surface targets** match the
  discovered eligible inventory. Listing targets is not runtime coverage.

## Functional runner qualification

An isolated Linux KVM host with 8 vCPUs and 32 GiB RAM passed all six selected
scenario targets using two invocation-owned Kind stacks and the Bazel-built
artifact image. Both stacks passed the existing baseline smoke before scenarios
ran. The targets cover ECW command/file round trips and pause/connect, native
command round trip and validation, and recovery after a data-plane restart.

| Scenario/API surface | Scenario execution (excludes bootstrap) |
| --- | ---: |
| ECW command round trip | 7.8s |
| ECW binary file round trip | 8.7s |
| ECW pause/connect | 18.9s |
| Native command round trip | 9.2s |
| Native command validation | 4.6s |
| Native command survives data-plane restart | 21.6s |

The same two worker IDs served subsequent targets, demonstrating stack reuse.
The pool invocation took **15.9 minutes**, including Bazel startup, two cold
bootstraps, the six scenarios and cleanup; tools-image provisioning is outside
that timer. Cold setup accounted for roughly 14 minutes on each stack. This
qualification does not measure a full-suite speedup or isolate machine size from
nested-container, cold-image and provisioning costs. First-target Bazel timings
include setup; the scenario timings above come from the runner's separate timer.

This covers six of 101 functional targets, not the whole suite. Full coverage
still needs the remaining targets and a host with the encrypted restore tests'
device-mapper prerequisites. No production pipeline or merge gate is switched by
this experiment.

The initial aggregate layer produced a 704 MiB archive. Separate gzip-compressed
layers produce about 274 MiB, versus about 270 MiB for the Cargo reference, and
avoid recompressing unchanged binaries. Packaging/configuration mismatches were
corrected before accepting the table above.

Raw accepted measurements are retained outside the public checkout under the
task's `comparison-v3` and `comparison-cli` directories; `compare.py`,
`audit_graph.py` and `validate.py` reproduce the protocol. Earlier development
trials with mismatched packaging or failed generation are excluded.
