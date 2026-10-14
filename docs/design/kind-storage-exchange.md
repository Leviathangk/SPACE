# Kind storage credentials through credential-manager's exchange (prototype)

Status: prototype write-up. It records what the storage exchange needs from an
object store, which S3-compatible stores provide it, and what the Kind
prototype on this branch does. It is meant to be dropped or reshaped before
anything merges.

## Problem

Deployed node services never hold object-store keys. volume-manager asks its
node's credential-manager agent (`--role=agent`) for credentials; the agent
forwards to the control-plane authority, which vends a short-lived session
through STS. build-manager calls the authority directly, and
cluster-controller uses workload identity.

Kind skips all of it:

- volume-manager and build-manager read static `SPACE_OBJECT_STORE_STATIC_*`
  keys (`infra/space/kind/charts/space-node-service/templates/_helpers.tpl`,
  `space-volume-manager-local/templates/daemonset.yaml`), which bypass the
  exchange (`crates/libs/vol/mgr/server/src/run.rs`,
  `crates/libs/build/mgr/src/object_store.rs`).
- cluster-controller reads the same keys as `AWS_ACCESS_KEY_ID`.
- With no `SPACE_CREDMGR_S3_VEND_ROLE_ARN`, the authority leaves
  `ExchangeToken` disabled (`crates/libs/credential/mgr/server/src/run.rs`).

Since the scoped storage exchange became the only `ExchangeToken` path (#233),
nothing tests that path, or its scoping, end to end.

## The exchange path, end to end

### Caller side

volume-manager's transfer executor asks for credentials per job
(`crates/libs/vol/mgr/server/src/executor.rs`, `exchange_credentials` in
`crates/libs/vol/mgr/server/src/credentials.rs`):

- `resource` names one exact object, `arn:aws:s3:::<bucket>/<key>`. The
  executor always passes `is_prefix = false`.
- `scope` is `space.storage.read` for a pull and
  `space.storage.read space.storage.write` for a push. A push needs read as
  well because it verifies the upload with HeadObject before committing it.
- `subject_token` is the SPACE subject (`sandbox:<id>` or `template:<name>`).
- `requested_expires_in` is `STORAGE_SESSION_TTL_SECONDS`, one hour
  (`crates/libs/credential/mgr/client/src/lib.rs`).

volume-manager reaches the agent's volume socket, the only listener that
admits the volume-manager identity. The agent replaces any actor claim with
the caller's UDS identity (`forwarded_exchange_request`,
`crates/libs/credential/mgr/server/src/node_service.rs`). It then forwards
upstream as the node credential agent, presenting its projected
ServiceAccount token.

The returned `AwsCredentials` are used as-is against the endpoint recorded in
the object's storage URI (`endpoint=`, `path_style=`). The S3 endpoint is
therefore data, not SDK configuration: `AWS_ENDPOINT_URL_S3` plays no part.

build-manager follows the same contract, but dials the authority itself
(`crates/libs/build/mgr/src/object_store.rs`). Its subjects are templates and
build inputs (`urn:space:token-type:build-input-sha256`).

### Authority side

`exchange_token` (`crates/libs/credential/mgr/server/src/service.rs`) runs these
steps in order:

1. **Caller identity** (`crates/libs/credential/mgr/server/src/k8s_identity.rs`).
   A TokenReview of the projected token checks four things: the pod must still
   have the token's UID and be Running; the connection must come from that
   pod's IP; and the token's node must be registered in `node_k8s` under both
   its name and its UID (`registered_node`,
   `crates/libs/common/svc/src/sql/node_registration.rs`).
2. **Contract and authorization** (`authorize_exchange`,
   `crates/libs/credential/mgr/server/src/storage_exchange.rs`):
   - It checks the RFC 8693 fields and the exact TTL, and resolves the
     effective actor.
   - For a sandbox subject, the key must be that sandbox's
     `suspend_snapshots/<shard>/<sandbox>/<snapshot>[/<artifact>]` path.
     Writes are fenced to the nodes that durably own the sandbox or snapshot.
     The object must sit inside the org's configured or recorded snapshot
     storage.
   - For a template subject, the key must be
     `templates/<name>/<hash>[/<file>]`, and writes to an already-published
     release are refused.
3. **Role.** The platform role is `SPACE_CREDMGR_S3_VEND_ROLE_ARN`. An org's
   row in `org_storage_vend_identities` overrides it.
4. **Session policy** (`AuthorizedExchange::session_policy`), at most 2048
   bytes, with two statements:
   - `s3:GetObject` (read), plus `s3:PutObject` and `s3:AbortMultipartUpload`
     (write), on exactly the requested object ARN.
   - `s3:ListBucket` on the bucket, conditioned on `s3:prefix` equal to the
     object key, or `StringLike` the prefix and `prefix/*` for a prefix
     resource.
5. **STS** (`assume_s3_vend_role`): `AssumeRole` on that role, with session
   name `space-storage-exchange`, `DurationSeconds` equal to the TTL, and the
   session policy as `Policy`. There are no session tags, external ID or
   source identity, so the session policy is the only scope. The authority
   rejects a result without an expiry, and emits one
   `credential_security_audit` event per decision.

The STS client comes from `aws_config::load_defaults` and
`AssumeRoleProvider::configure(&sdk_config)`, which builds the STS client with
`aws_sdk_sts::config::Builder::from(&sdk_config)`. Three things therefore come
from the SDK's ordinary configuration:

- The STS endpoint: `AWS_ENDPOINT_URL_STS`, then `AWS_ENDPOINT_URL`.
- The region: `AWS_REGION`.
- The base credentials for the AssumeRole call: the default chain. That is
  static environment keys in Kind; deployments use their workload identity.

No SPACE-specific endpoint setting exists or is needed.

`SPACE_CREDMGR_SCOPED_EXCHANGE_TOKEN_MODE` is retired. The authority logs and
ignores any value except `disabled`, which fails startup while exchange is
enabled (`check_retired_exchange_token_mode`, `run.rs`). A deployment that
still sets `enforce` gets the same behaviour as one that sets nothing, so Kind
does not set it.

### What an honest store must do

To prove the scoping rather than only the plumbing, the store has to:

1. Implement STS `AssumeRole` signed with an IAM user's keys, honour
   `DurationSeconds`, and accept the ARN syntax the authority validates
   (`arn:aws:iam::<12 digits>:role/<name>`, `iam_role_arn.rs`).
2. Validate the role. Ideally it refuses a role that does not exist or does
   not trust the caller, so a wrong org override fails as it would on AWS.
3. Evaluate the session policy as an intersection with the role's own policy
   on every S3 request:
   - resource ARN matching on the exact key;
   - the `s3:prefix` condition on `ListObjects(V2)`;
   - AWS's action mapping, where `CreateMultipartUpload`, `UploadPart`,
     and `CompleteMultipartUpload` fall under `s3:PutObject`, and
     `HeadObject` under `s3:GetObject`. volume-manager streams snapshot
     uploads as multipart, so a store that maps these differently fails every
     push.
4. Reject an expired or forged session token.

## Candidate stores

Every candidate except Ceph was probed on this box with the same script. It
replays the authority's exact session-policy shape (string `Resource`, array
`Action`, `StringEquals` on `s3:prefix`) for one snapshot object, then tries
GetObject, HeadObject, PutObject, a 24 MiB multipart upload,
AbortMultipartUpload, ListObjectsV2 and DeleteObject both inside and outside
the granted key, and retries with a forged session token.

| Store | STS operations used by the authority | Session policy enforced on S3 | Role validated | License | Image (amd64, compressed) / idle memory / ready | Fit with Kind's object-store chart |
|---|---|---|---|---|---|---|
| versitygw v1.8–v1.9 (today's store) | No `AssumeRole`. The separate standalone IAM service added in v1.8.0 serves only `AssumeRoleWithWebIdentity` and `GetCallerIdentity`. | Policy evaluation exists for those sessions; untestable here without `AssumeRole`. | n/a | Apache-2.0 | ~31–36 MB / small / ~1 s | Already the store. Would need the authority to change its STS call, which is out of scope. |
| SeaweedFS 4.48 | `AssumeRole` with `Policy` and `DurationSeconds`, signed by a configured identity, and allowed only by the role's trust policy. | Yes: all probes behaved as AWS would, including multipart (expanded from `s3:PutObject` since 4.19) and the `s3:prefix` condition. | Yes: an untrusted role ARN is refused (`AccessDenied ... not authorized to assume role`). | Apache-2.0 | ~194 MB (~724 MB unpacked) / ~230 MiB / ~13 s to `/healthz` after a restart | Kind ran SeaweedFS before versitygw. It was replaced because of volume-slot exhaustion and the master/volume/filer start-up time, which need the sizing flags the old chart carried. |
| RustFS 1.0.1 | `AssumeRole` with `Policy`, MinIO semantics: the session gets the parent user's policy intersected with the session policy. | Yes: all probes behaved as AWS would, including the string `Resource` an earlier alpha rejected. | No: `RoleArn` is ignored, so any well-formed ARN succeeds. | Apache-2.0 | ~111 MB / ~150 MiB / seconds | Single process. Fits the chart like versitygw does. A per-org role override cannot be told apart from the platform role. |
| MinIO | `AssumeRole` with `Policy` (MinIO semantics). | Yes, historically. | No (MinIO ignores `RoleArn`). | AGPL-3.0 | Not available: the community edition stopped publishing images and binaries in October 2025 and has since been archived. | Not viable: no maintained image, and running a source build in CI would make SPACE maintain it. |
| Ceph RGW (`quay.io/ceph/ceph` v19) | `AssumeRole` with `Policy`, plus `AssumeRoleWithWebIdentity`. | Documented as enforced (role policy intersected with session policy). Not probed: needs a full mon/mgr/osd cluster or the experimental single-node backend. | Yes (roles and trust policies). | LGPL-2.1/3.0 | ~490 MB compressed, plus several daemons and GiBs of memory | Heaviest option, and a new failure surface on the infra node. |
| moto server | `AssumeRole` (returns credentials). | **No.** IAM checks are opt-in, and an assumed role's permissions come only from the role's own inline and attached policies (`AssumedRoleAccessKey.collect_policies`, `moto/iam/access_control.py`). The session policy is never consulted, and the module lists Resource and Condition support as TODO. | Partly | Apache-2.0 | ~104 MB / small | Would pass every scoping test without proving anything. Also keeps objects in memory. |
| LocalStack | `AssumeRole`. | Only with `ENFORCE_IAM=1`, a paid feature. | Paid | Mixed (the free edition is no longer a separate image) | `localstack/localstack:latest` needs an account auth token since March 2026 | Not viable for public CI without an account. |

### Recommendation

**SeaweedFS (pinned at 4.48 or later)** is the only candidate that does all
four things an honest store must, with a license and image SPACE can run as a
test service. In particular, it validates the role against its trust policy,
so the authority's role choice is exercised and not just carried through.

Its costs are known from Kind's earlier use:

- a larger image, which CI must mirror;
- tens of seconds to start under load;
- the all-in-one process needs the sizing flags (`-master.volumeSizeLimitMB`,
  `-volume.max`) and the 4 GiB memory limit the old chart carried;
- its data does not move to or from versitygw.

**RustFS** is the fallback if SeaweedFS's operational costs return. It enforces
the session policy just as well and is lighter. But it cannot tell one role
from another, so it proves "the session policy is the scope" and not "the
authority picked the right role".

The alternative to both is an **in-repo STS fixture in front of versitygw**,
like the key-broker fixture. To be honest, it would have to:

- terminate SigV4 for both STS and S3, including aws-chunked streaming
  payloads and `UNSIGNED-PAYLOAD`;
- mint and expire session credentials, and refuse forged or expired tokens;
- evaluate the session policy per request with AWS's action mapping
  (multipart under `s3:PutObject`, HeadObject under `s3:GetObject`), ARN
  matching and the `s3:prefix` condition;
- validate role ARNs against a trust list;
- re-sign the allowed request to versitygw.

That is a security-relevant SigV4 implementation SPACE would own only for
tests, and a bug in it would make the scoping proof worthless. Use it only if
no third-party store remains viable.

## The Kind prototype on this branch

It is enabled with `SPACE_KIND_STORAGE_CREDENTIALS=exchange` on a fresh
cluster. The default, `static`, is today's behaviour. Both bring-up paths
(Tilt and `bootstrap.sh`) read the same setting through
`infra/space/kind/lib/storage-credentials.sh`.

In exchange mode:

- **Object store.** `space-object-store` runs SeaweedFS behind the same
  Deployment, Service (`object-store:8333`), Secret and bucket-init Job.
  - The root identity keeps the existing admin keys. The bucket-init and
    seed-publish Jobs, build-manager and cluster-controller still use them.
  - A second identity holds no S3 permissions and may only assume the
    `kind-storage-vend` role.
  - That role's attached policy covers the `snapshots` and `templates`
    buckets, and its trust policy names only that identity.
  - The data lives under its own hostPath. The transition check refuses to
    switch an existing cluster between stores, because neither store can read
    the other's data.
- **Authority.** `space-credmgr-authority` gets:
  - `SPACE_CREDMGR_S3_VEND_ROLE_ARN` for that role;
  - the vend identity's keys as its default-chain credentials;
  - `AWS_ENDPOINT_URL_STS=http://object-store:8333`.

  These are the settings a deployment uses; only the values are Kind's. It no
  longer maps `space-aws-creds`, because one default chain cannot serve two
  accounts.
- **volume-manager.** On sandbox workers it gets no object-store keys, so every
  snapshot and template transfer goes volume-manager → node agent → authority
  → SeaweedFS STS → scoped S3 session.
- **Builders.** The builder release keeps static keys, because the authority
  refuses builder-node callers (next section). build-manager is unchanged for
  the same reason.
- **cluster-controller.** It keeps static keys in place of workload identity.

## Builder nodes fail the authority's node check

This is a code-level finding. The authority admits a Kubernetes caller only if
`node_k8s` holds the token's node under its name **and** UID
(`check_node_registration`, `k8s_identity.rs`, introduced with pod, node and
peer binding in #185). Two writers record that UID:

- the node-inventory sync, which lists only nodes carrying the sandbox node
  selector (`sync_labeled_nodes_to_db`,
  `crates/libs/api/src/controllers/substrate/kubernetes/node_sync.rs`);
- sandbox pod placement.

internal-api's registration (`bootstrap_k8s_origin_node`,
`crates/libs/internal/api/server/src/lib.rs`) writes `k8s_node_uid = NULL`.
Builder nodes carry no sandbox selector and host no sandbox pods, so their
registration never gains a UID. The authority therefore refuses every caller
on a builder node with `node_identity_mismatch`: build-manager, and the
builder's credential-manager agent forwarding for its volume-manager.

### Proposed fix

The node binding exists so that a token reviewed for node N, with UID U, is
honoured only while SPACE's registration of N refers to the same Kubernetes
object. A recreated node under the same name must not inherit the old one's
access. The UID must come from the Kubernetes API, not from the node.

The fix keeps that, and changes three things:

- After its sandbox-selector pass, the inventory sync lists SPACE's own
  Kubernetes-managed builder registrations. It does not select Nodes by
  label: builder pools are labelled differently from one environment to the
  next, and nothing sets `space.pplx.net/node-role` on them.
- For each registration it reads the Node by name from the API server, as
  the sandbox pass reads its Nodes, and **binds only that Kubernetes UID onto
  the registration internal-api already made**. It updates
  `node_k8s.k8s_node_uid` where the name matches, and clears drain state when
  an already-bound UID changes, exactly as the sandbox-node upsert does. It
  neither creates builder rows nor takes over their lifecycle state, which
  stays with node-ctl's publishing.
- A builder that was never registered stays unregistered, and is still
  refused.

Two alternatives were rejected:

- **Recording the UID in internal-api's registration** would make it
  node-asserted. The registering node-ctl would supply its own UID, which the
  check exists to verify independently.
- **Extending the sandbox selector to builders** would make the sync manage
  builder lifecycle state and demotion, which node-ctl owns.

The fix is on `ac/kind-fidelity-sts-prototype-fix-builder-uid`. With it,
Kind can drop the builder's static keys and point build-manager at the
authority, the stretch goal of this prototype. That takes three changes:

- The builder volume-manager release joins the worker one in getting no
  keys.
- build-manager gets the authority's endpoint and CA
  (`space-node-service.env.credmgrAuthority`) and the projected credmgr
  token in place of the static keys.
- The authority's allow list maps `space-builder-build-manager` to
  `space-build-manager`.

These changes stay off this branch until the fix merges, because exchange
mode would otherwise break every template build.

## What the prototype showed on Kind

The prototype ran on a fresh exchange-mode cluster on a Linux/KVM devbox,
with the smoke suite and the scenarios that move snapshots and templates
through the object store. The PR has the results table and timings.

- **The path works end to end.** Worker volume-manager pulled the seed
  template, pushed and pulled suspend snapshots, and copied fork snapshots,
  all with sessions from the authority. Each one appears as a
  `credential_security_audit` `issued` event and as an `STS.AssumeRole`
  client span inside volume-manager's job trace. No exchange was denied and
  no role assumption failed.
- **The scope is enforced by the store.** A session vended through the real
  chain for one sandbox's snapshot object read that object. SeaweedFS
  refused it for another sandbox's object (HeadObject, GetObject, and
  ListObjectsV2 on that sandbox's prefix), for a write under read scope, and
  for the templates bucket. The authority itself refused to vend the first
  sandbox's subject for the second sandbox's object.
- **The builder finding reproduces, and the fix resolves it.** A caller on a
  builder node was refused with `node_identity_mismatch` on the starting
  commit and on this branch. With the fix deployed on top, the builders'
  registrations gained their UIDs and the same caller passed every node
  check. build-manager, calling with its own identity, received a scoped
  session. With the builder changes above, the OCI template-build scenario
  passed, its build-input and template reads and writes all vended through
  the exchange.
- **Cost.** The object store took about 14 s from pod creation to Ready,
  including its signing-key init container. That compares with about 1 s for
  versitygw. SeaweedFS held about 700 MiB after the seed publish.

## What stays different from deployments

- The authority's base credentials are static keys of the store's vend
  identity, not workload identity.
- Builder volume-manager and build-manager keep static keys until the builder
  UID fix lands.
- cluster-controller keeps static keys instead of workload identity.
- STS and S3 are one SeaweedFS endpoint, and there is no AWS account
  boundary.

## Decisions for the user

1. Make exchange mode Kind's default, or keep it opt-in? Making it the default
   means CI mirrors SeaweedFS and every Kind run exercises the exchange.
2. Accept SeaweedFS's operational cost, or take RustFS and lose role
   validation?
3. Land the builder UID fix, then convert build-manager?
