# Controller image comparison

The complete candidate works, but this run does not justify replacing the existing
build for speed. Its cold build and controller edit are slower. An unrelated Rust
edit avoids compilation under Bazel, but restoring its action outputs into a new
checkout still has a noticeable cost.

| Case | Cargo + sccache + BuildKit | rules_rs + LLVM + rules_oci |
| --- | ---: | ---: |
| Cold compiler/action cache | 287.2s | 359.7s |
| Unchanged, same checkout | 1.4s | 1.7s |
| Unrelated Rust edit, new checkout | 41.2s | 26.9s |
| Controller Rust edit, new checkout | 40.8s | 65.2s |
| Revision only, new checkout | 2.3s | 29.9s |
| Fresh worker/checkout, seeded reference caches | 24.2s | 29.2s |

## Fixed inputs and measured boundary

Source: `9d356f0ac3ff35ef23ff8c487edff99c4381ef4f`, Linux/amd64, Rust 1.98.0,
16 jobs, Cargo release optimization and host build profiles, `lto=false` including
local ThinLTO, and the same runtime/provenance contract. Both paths export a full
Docker-format image archive. Dependency/repository setup is included; source
snapshot creation, builder bootstrap, image loading, publication, and cache export
are excluded. Caches use local storage.

The old path retains GCC/Wild; the candidate uses LLVM. These are complete-stack
measurements. Compiler/archive assets are locally available from preparation, so
cold describes compilation/action-cache state rather than a machine with no tools.

The changed-source rows use fresh checkouts on the existing workers. The two paths
receive byte-identical edits. Unique markers avoid hits on earlier trial inputs.
The fresh-worker row creates a new BuildKit builder and a new Bazel checkout/output
base. Both reuse the complete reference caches. The old compiler-cache mount is
primed before timing with an unchanged measured seed bind, preserving layer keys.
The revision-only row uses a synthetic revision; it is not a private Trunk build.

## Evidence

- All 691 distinct Cargo release compilation units are present in the executed
  candidate profile, with matching features and optimization. The audit expands
  response files and reports zero missing units and zero differences. Four Bazel
  internal tooling units are outside the Cargo profile.
- Cargo target names and package environment values are preserved. The controller
  Rust crate is `space_cluster_controller` on both paths.
- Final image configuration, CLI help/version, ELF type, and runtime filesystem
  content/metadata match, excluding controller binary bytes and Docker host files.
  This is build/artifact validation, not a controller integration-test run.
- The old cold build spent 94.3s cooking dependencies and 190.9s in the final release
  build. Its cold trial repeated at 287.2s versus the earlier 288.2s observation.
- Bazel executes no Rust compilation for unrelated, provenance, or fresh-cache
  cases. It executes one Rust compilation for the controller edit. That one unit
  still includes code generation/linking; cached inputs do not remove this cost.
- These are single accepted trials on one worker. Subsecond differences in the warm
  row and modest fresh-cache differences should not be treated as stable effects.

## Reproduction and raw results

See [README.md](README.md) for preparation, trials, and parity validation.
The accepted Cargo samples are under `.cache/controller-benchmark/matched-v2/`;
accepted candidate samples are under `.cache/controller-benchmark/matched-v3/`.
`matched-v3/comparison.json` lists each sample's absolute log location, action
counts, runtime digest, and parity results. Earlier candidate and repeated-input
samples are retained separately and excluded from this table.

To pair the final candidate with the old changed-source samples, the candidate
cases used `--run-id matched-v3 --variant final --input-run-id matched-v2`.
The input-run override only selects the synthetic edit/revision marker. It does
not select another compiler cache. New experiments should use a new run ID on
both backends and retain that same ID for their paired cases.
