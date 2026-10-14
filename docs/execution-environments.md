# Execution environment identities

[Documentation](../README.md)

An execution environment binds a runtime, its host requirements, a guest
platform, boot inputs, isolation semantics, and an adapter compatibility profile.
Its reference is the SHA-256 of that complete definition. An artifact digest
identifies bytes; an environment reference identifies a particular combination
of bytes and execution requirements. Neither reference implies that those bytes
are installed, usable, or approved for execution on a node.

This first contract supports the compiled **E0** definition: Linux/x86_64 host,
KVM, Firecracker 1.15.1, Linux/x86_64 guest, kernel 6.1.155, a hardware VM boundary,
and Firecracker snapshot format 9.0.0. The runtime digest covers the pinned
Firecracker and jailer binaries, not their download archive.

The implementation includes domain types, protobuf messages, a versioned JSON
codec, and explicit E0 seed/build binding. Node-manager resolves only verified
compiled E0; this does not activate another environment, add policy tables, or
change execution defaults. Experimental runtime/kernel selection follows this
binding protocol.

## Ownership and extension boundaries

| Concept | Meaning and owner |
|---|---|
| `HostPlatform` | Host OS family plus architecture. Policy cohorts must use both. Linux, Windows and macOS are distinguishable identities, not claims that this build implements their execution adapters. |
| `GuestPlatform` | Existing guest OS family plus architecture. It is independent of the host. Distribution/release, image provenance and mutable boot state remain template/sandbox concerns. |
| `RuntimeBinding` | A tagged provider reference. Schema 1 implements only `ManagedRelease`, with an exact binary digest and host target. A host facility must get a distinct provider when its adapter is implemented; it must not be represented by an invented installable artifact. |
| `BootProfile` | Versioned adapter semantics and immutable boot components. `LinuxDirectV1` names a required guest kernel, targeted at the descriptor's guest platform. The root filesystem comes from the template. Firmware/disk boot and mutable NVRAM are not encoded speculatively. |
| Backend, payload layout, isolation | Typed vocabularies with only KVM, the Firecracker release layout, and a hardware VM boundary implemented. Isolation classes are categories for allowed-class policy, never a numeric ranking. Unknown/unspecified values are unsupported. |
| `ExecutionCompatibilityProfile` | Versioned adapter interpretation of compatibility requirements. `FirecrackerSnapshotV1` retains the current `StateFormatIdentity` meaning. It does not promise that arbitrary Firecracker versions can restore each other's snapshots. Machine/device compatibility and per-operation capabilities belong to their first adapter consumer. |

Schema 1 describes the existing SPACE Firecracker direct-boot adapter. Its
command-line construction and device configuration remain adapter-owned; CPU,
memory, disks, and guest userspace remain workload/template inputs. Any future
environment-selectable boot or device configuration must participate in a new
profile/schema identity before it can affect execution. Adding a field that
changes boot or restore meaning while preserving the old reference is invalid.

Structural decoding and execution approval are separate:

1. `EnvironmentDescriptor::try_from` requires complete fields and consistent
   host, runtime target, guest and implemented profiles. It currently rejects
   non-Firecracker/non-Linux combinations, rather than pretending the existing
   profile describes them.
2. `EnvironmentDefinition` computes the content reference. Wire/document readers
   recompute it and reject a mismatched claimed reference.
3. `InitialEnvironment::resolve_definition` resolves only the compiled E0
   reference. A well-formed candidate descriptor or approved downloadable
   artifact does not expand that set. Existing aarch64 installation pins retain
   their old meaning; production qualification covers only the x86_64 E0 descriptor.
4. Execution consumers must separately verify node usability and allowed
   selection. No caller may turn successful decoding into execution permission.

No source URL, cache/vendor path, environment variable, operator label, observed
health, enforcement mode, or installation job revision is part of this identity.
Local source/path configuration cannot become a second selection authority.

## Canonical identity and stored representation

The schema-1 canonical encoding is defined by
[`canonical_descriptor`](../crates/libs/execution/common/src/environment.rs)
and the independent [E0 golden vector](../proto/execution/v1/fixtures/e0.canonical).
It uses UTF-8 ASCII text, a domain separator, schema version, fixed field order,
canonical scalar spellings, and a final LF. Identifiers exclude delimiters and
newlines; digests use lowercase `sha256:` plus 64 hex digits. Schema 1 has named
runtime and kernel slots, not unordered artifact arrays. A future collection
needs an explicit ordering/duplicate rule in its new schema.

Hash the exact canonical bytes with SHA-256. E0's reference is:

```text
sha256:3168c96b5719ce62543ae53e6641a4eb80416a9f01e5585f6f76c3c70111fa0b
```

The [JSON fixture](../proto/execution/v1/fixtures/e0.json) is the versioned stored
representation: `{schema_version, reference, descriptor}`. JSON ordering and
whitespace do not affect identity. The stored codec requires canonical scalar
spellings, rejects duplicate/unknown fields (including nested legacy types), and
verifies the reference on every load. Protobuf conversion validates required
messages, enums and oneofs, rejects noncanonical digest spellings, and checks the
same reference. JSON/protobuf serialization bytes are never hashed.

The first owning policy store must persist the immutable definition and reference
consistently and check them on load. It must use forward migrations and preserve
authoritative execution records and retained jobs. This change defines that
document boundary without adding unused SQL tables or modifying applied
migrations. Document round-trip tests establish codec compatibility, not a claim
that a database backfill or end-to-end rollout has been exercised.

## Rollout and unknown data

Existing `ExecutionMetadata`, inventory messages, and enum field numbers retain
their wire and JSON meanings. They are not evidence of environment support and
cannot be converted into an approved environment merely from a version string.
Legacy-to-E0 attribution must be implemented by the binding consumer under the
stock-image compatibility rules; missing or ambiguous evidence must not become
an inferred Linux/E0 binding.

New environment consumers require positive evidence that the peer supports both
the schema and the qualified environment reference. The builder now requires
`ResolveExecutionEnvironment` to return a checked definition and matching bound
execution metadata; `Unimplemented`, missing fields, and conflicting evidence
fail the build. Sandbox-manager requires a bound prepare request to receive the
same reference before persisting the response or launching. An old node-manager
that drops the field cannot acknowledge a bound request.

Protobuf readers normally discard unknown fields. Therefore **any new required
meaning**, including a new field or provider/profile arm, requires a new
environment schema version. Readers reject unknown/zero schema versions, unknown
enums, and absent/unknown required oneofs. A sender cannot rely on an old reader
to notice an extra field under schema 1. Additive transport compatibility does
not substitute for explicit capability negotiation. Do not infer support from
successful protobuf decoding or the old installation projection.

Tests cover frozen legacy metadata wire bytes, E0 wire/document round trips,
reference mismatches, missing/unknown values, unsupported combinations, and
catalog rejection of well-formed but unqualified candidates. Binding tests also
cover missing peer acknowledgement, contradictory execution observations,
manifest publication, SQLite JSON round trips, and PostgreSQL write-once storage. A real Linux/KVM build,
publish, pull and boot remains the deployment acceptance test.

## Seed/build binding and deployment

`ResolveExecutionEnvironment` is a read-only node-local RPC with the node-status
peer allowlist. An omitted reference requests compiled E0; an explicit reference
must resolve to that same approved definition. Node-manager requires Linux and a
non-overlapping managed-store layout, verifies E0 through the execution inventory,
and returns the definition with matching execution observations. Prototype layouts
that retain unverified external execution cannot produce this binding. The RPC
is an observation, not a reservation or an activation operation; prepare and
launch resolve again. E0 artifacts remain protected from removal.

The builder distinguishes verified bindings from legacy execution before resolution:

- An unpinned scratch build on Linux/x86_64 resolves E0 and receives its runtime,
  state format and kernel identity. Custom-kernel recipes and ARM scratch builds
  retain their unbound contract and supplied kernel pin. Neither path scans for
  the newest runtime, and inherited selector variables are overwritten. Binding
  changes only the execution/schema fields, preserving seed image provenance.
- Derived builds require readable base and actual sandbox execution metadata.
  An inherited environment reference must resolve and match both observations.
  Unbound bases remain unbound; missing kernel identity is not silently attributed
  to E0. Kind's bootstrap seed records the actual staged kernel digest so new
  seeds carry complete evidence. Missing execution metadata still fails closed.

Resolver errors never downgrade a bound build to legacy execution. Constructing
an image records its target; it is not evidence of a successful VM boot.

New bound `metadata.json` and `manifest.json` use schema 3 and carry
`execution.environment`. Schema 2 remains the unbound shape. A schema/binding
mismatch fails validation; the old schema-2 reader rejects schema 3. The
reference survives normalization, immutable manifest publication, the execution
JSON stored by template-manager and the control plane, worker preparation, and
sandbox metadata. No table migration or backfill is required. The reference is
not a substitute for binary verification or an arbitrary execution permission.
A same-rootfs registration cannot add, remove or replace the retained manifest's
binding by refreshing metadata; it fails before changing the existing release.
Control-plane registration likewise rejects contradictory execution metadata
before changing tags or storage locations. Identical retries and metadata-free
tag updates preserve the original evidence; a NULL record may be filled once.

Deploy the new control-plane, template-manager, sandbox-manager and node-manager
readers before enabling builds with the new builder. Until readers are upgraded,
new builds or bound-template consumption may fail closed; existing unbound
workloads retain their previous execution behavior. Production qualification
covers only Linux/x86_64 E0; ARM builders continue producing unbound templates.
A legacy base needs complete matching evidence before opting into a binding.
Do not strip a binding to make a template load on an old reader.

For rollback, stop producing bound releases and use retained schema-2 releases
with the old services. Keep compatible readers while bound templates or their
sandbox/snapshot records remain in use: older JSON/protobuf readers outside the
template schema gate can discard the new field. This change does not claim
fleet-wide capability placement or snapshot-version negotiation. Experimental
benchmarks must add their own node admission and isolate pools/caches by
environment; cluster-wide provisioning can follow separately. Installing 1.17.0
still does not select it.

## Bounded audit of existing contracts

This audit concerns the six boundaries touched by the environment-contract and
provisioning work. It is based on the repository schema and readers/writers,
not inspection of live deployment versions. The owning implementation must
establish deployed migration/peer versions before a cutover. None of the
deferred replacements below authorizes destructive resets or reinterpretation
of existing data.

| Existing boundary | Disposition and target semantics | Owner and delivery | Compatibility/removal criterion |
|---|---|---|---|
| `RuntimeIdentity`, `RuntimeInstallation`, `NodeExecutionInventory`; PostgreSQL `node_runtime_installations` keyed by `(node_id, runtime, runtime_version)` and its dependent projections | **Keep** release metadata. **Replace with first consumer** its use as sufficient environment capability: observations need complete environment reference plus host/usability evidence. This PR adds the separate definition/reference, not a renamed installation row. | Execution-common owns definitions; node-ctl publication and cluster storage own the later provisioning projection; placement migration follows publication. | Preserve old inventory for old readers during rollout. New admission requires new evidence. Remove the old admission dependency only after every reader migrates; old release diagnostics may remain useful. |
| `GuestOperatingSystem`, `GuestPlatform`, `ArtifactComponentKind` | **Keep** their current meanings. **Replace now** missing host identity with separate `HostPlatform`; do not infer host OS from guest OS. | Common-core/execution-common, this contract. Template owner adds distribution/release with its first guest-image consumer. | Additive messages preserve old guest fields and enum numbers. There is no guest-field migration/removal in this change. |
| Runtime-manager `AvailableArtifact.architecture` and `ExecutionArtifact.architecture` | **Keep** legacy architecture evidence; it is not a full target or placement requirement. **Replace now** in new descriptors with explicit host target for runtime bytes and guest platform for the direct-boot kernel. | Execution-common owns descriptor targets now; runtime-manager owns richer diagnostic target fields with the first additional installer/target. | Do not silently change the old strings' meaning. Retire their placement use when consumers use typed targets; keep diagnostic evidence as needed. |
| `ExecutionMetadata`, node `requested_execution`, string runtime selectors and CLI `--runtime fc` | **Keep** current adapter selection/recorded metadata. **Replace with first consumer** as complete selection authority: explicit environment references and actual resolved execution metadata through seed/build, then benchmark launch. | Template/build and node-manager owners: E0 binding is implemented here; explicit candidate selection requires opt-in benchmark admission. | Validate legacy aliases and reject contradictory old/new selectors at the consuming boundary. Unbound legacy paths remain unchanged until stock-image attribution is implemented; removal requires migrated writers/readers and durable records. |
| Runtime-manager SQLite `operation_results`, shared `jobs`, `InstallOperation` JSON and dedupe | **Keep** `install_kernel`, `uninstall_kernel`, `install_release`, `uninstall_release` as accurate existing operations. Environment policy does not change their identity or replay semantics. | Runtime-manager owns the first new operation kind and any versioned payload migration. | Preserve retained-job decode/replay and `(action, artifact identity)` dedupe; policy revision is correlation only. Verify deployed migration level before evolution. The historical prototype reset is not a future migration strategy. |
| Firecracker paths, sources, local catalog JSON and POSIX protection configuration | **Keep** inside the Linux/Firecracker installer/store adapter. Portable policy references approved environments, not paths or arbitrary hashes. New identity has no ENV selection authority. | Node-manager/runtime-manager own adapter configuration; provisioning/source integration owns managed-store/bootstrap precedence. | Existing configuration keeps its semantics in this PR. The first replacement must define precedence and remove old knobs only after bootstrap, restart and rollback consumers migrate. |

Schema anchors:
[`PostgreSQL baseline`](../db/postgres/20261002070625_baseline.up.sql),
[`runtime-manager artifact operations`](../db/runtime-manager/20261012064915_artifact_operations.up.sql),
[`durable operation payloads`](../crates/libs/node/runtime/manager/server/src/installs/operations.rs),
[`execution protocol`](../proto/execution/v1/execution.proto), and
[`artifact diagnostics protocol`](../proto/node/runtime/manager/v1/manager.proto).

The artifact catalog's kernel/runtime-line association is source/compatibility
evidence, not a new kernel identity. Benchmark admission must validate exact
pairs without relabeling or duplicating the 6.1.155 pin. Lifecycle portability,
host-provided runtimes, firmware boot, guest release metadata and richer snapshot
compatibility remain owned by their first real adapter/guest consumer. These
follow-ups do not block this contract slice or expand it into a whole-platform
schema redesign.
