# Tombstone-retention extraction and Gazelle comparison

The extraction is buildable and tested, but this single small crate does not
reduce a production-edit rebuild. Gazelle's separation of test sources does
eliminate an unnecessary production rebuild after a test-only edit.

## Change

`crates/libs/cluster/controller/tombstone-retention` contains a flat `lib.rs`
and colocated `retention.test.rs`. Gazelle 1.2.1 owns its native `rules_rs`
library and test targets. The Cargo manifest retains equivalent library and
standalone-test entry points for the existing build path. The controller server
imports the extracted crate; defaults, SQL operations, loop behavior, and the
existing tracing target are preserved. SQL locator comments identify the new
crate path.

## Measurements

These are serial, warm same-checkout builds on the same worker, using Rust
1.98.0, 16 jobs, the existing Cargo/GCC/Wild stack and Bazel/LLVM stack, and local
caches. Each trial warms the tree before applying its edit and produces a
complete Docker image archive. Daemon loading is excluded. No separate
`bazel fetch`, fresh-checkout setup, CI queueing, or cluster startup is timed.

| Edit and build path | Before extraction | After extraction |
| --- | ---: | ---: |
| Production edit, Cargo+sccache+BuildKit | 86.8s | 86.7s |
| Production edit, Bazel | 77.5s | 78.7s |
| Test-only edit, Bazel production image | 44.0s | 0.9s |

The accepted production trials change the same batch-limit declaration from
1,000 to 1,002 in both layouts and both build paths. This is an actual production
source edit. The test-only trial adds the same comment to the retention test
file, exercising build invalidation without changing production behavior.
All sources are restored after the experiment.

Before the split, the production edit executes two Rust actions: the controller
server and its binary. After the split it executes three: the extracted library,
the server, and the binary. The extracted library takes approximately 0.25s;
the server still takes approximately 43s and the binary approximately 32s.
The changed dependency still invalidates those large downstream actions.

For the test-only edit, the older experimental `space_crate` rule's broad
source glob executes the server Rust action even though the test file is not
compiled into the production binary. The Gazelle-generated library lists only
`lib.rs`; after the split the test-only edit executes zero Rust actions.
This gain comes from correct production/test inputs. Existing crates could
adopt that partitioning without being split into additional crates. This is
not evidence that Cargo previously recompiled production code for test edits.

The production differences are small enough to treat as no demonstrated win.
These are individual trials, not statistical estimates. An earlier independent
Bazel edit (1,000 to 1,001) similarly measured 78.1s before and 78.3s after.

## Validation and evidence

- All six existing retention tests pass, including four tests against real
  Postgres with `SPACE_CI_REQUIRE_TEST_DEPS=1`. No dependency skips are accepted.
- Cargo and Bazel match all 692 compilation units in the split build, with no
  missing units or feature/profile differences.
- Image configuration, CLI output, ELF format, and runtime filesystem checks
  pass against the original controller image, excluding compiled binary bytes.
- Gazelle strict diff, Rust formatting, Python lint/format/syntax, Bazel file
  formatting, changed-file test placement, and whitespace checks pass.

Accepted timings and action logs live under
`.cache/controller-benchmark/split-comparison/{cargo-serial,bazel-serial,test-only-serial}`.
Compiler/image parity and test output live under
`.cache/controller-benchmark/split-checks/`. These are local benchmark artifacts;
the checked-in experiment contains no private pipeline data.

Use `split_benchmark.py` as documented in [README.md](README.md). Pass a fresh
`--batch-limit` when repeating production trials to avoid reusing a prior edited
artifact; use `--edit-kind tests` to check test-source isolation. Run the build
paths serially.
