# versitygw + STS bridge spike: report

## Verdict: GO WITH CONDITIONS

versitygw v1.9.0's standalone IAM service enforces everything the
credential-manager authority relies on: the exact-object and prefix session
policies, the role's identity policy as their ceiling, role trust, the role's
maximum duration and session expiry. A 425-line request path in a Kind-only
bridge turns the authority's unchanged `AssumeRole` into
`AssumeRoleWithWebIdentity`, and the authority's own `AssumeRoleProvider` code
gets usable, correctly scoped sessions through it.

Against the decision rule:

| Rule | Result |
|---|---|
| Every criterion-C check passes, plus expiry | **Not met as written: C21.** 46 of 47 in five of six runs, 47 of 47 in one. Expiry 4 of 4 on both paths. |
| Every trust check refused as it should be | Met: 19 of 19 (T1–T14 directly; B10–B14 through the bridge) |
| Forged credentials refused | Met: 9 of 9 (C36–C39b, F1–F4), with the genuine session still working (F5) |
| The bridge works end to end with the SDK's real `AssumeRole` call | Met: the authority's `AssumeRoleProvider` code, the SDK's `assume_role`, aws CLI v2 and `curl --aws-sigv4` |
| Session burst within 1.2× the static-key burst, no stalls ≥ 5 s | Met: 1.002×, 0 stalls, 0 failed uploads |

**The failed check, C21 (a read-only session's 20 MiB `put_file`).** versitygw
refused the write in every run: its log shows 403 AccessDenied for each
attempt, and C23 found the object unchanged in all six runs. What fails is the
observable result. versitygw answers before reading the body and closes the
connection, so SPACE's client (hyper, via aws-sdk-rust) usually fails its body
write with `Connection reset by peer`, retries twice as if the network had
failed, and never sees the 403. This is not a policy that went unenforced, and
the STS path does not introduce it. Kind's store has the same race today: with
static keys, versitygw v1.8.0 in Kind's static mode returned a 20 MiB PUT's
refusal as a reset in 10 of 20 tries for a wrong secret, and v1.9.0 did so in
20 of 20 (`results/early-refusals*.md`). Refused PUTs of up to 64 KiB returned
403 in every run (C11w–C14w, C20, C32–C35, B0c).

**Conditions** (each checkable):

1. Kind's assertions about out-of-scope writes (a negative proof such as the
   bake-off's `negative-proof.sh`) either use bodies of at most 64 KiB or assert
   that the object is unchanged, as C23 does. They must not assert on the HTTP
   status of a large refused PUT. Lift this once `early` (`results/early-refusals.md`)
   shows the status in 20 of 20 tries, after an upstream change or a client that
   reads early responses.
2. The pinned versitygw image is `v1.9.0@sha256:eb7f786f…fdc7`. Any other
   version (earlier ones were not tested with the IAM service) is adopted only
   after the probe's `exchange`, `expiry`, `trust` and
   `bridge-auth` subcommands pass on it, with C21 judged by condition 1.
3. The IAM service's `--dir` sits on the store's persistent hostPath, like the
   gateway's data. The vend role, the provider and every live session are
   stored there; sessions survived IAM and gateway restarts only because the
   directory did (`results/resilience.txt`).

## What versitygw needed

Two processes from the same image:
`ghcr.io/versity/versitygw:v1.9.0@sha256:eb7f786fcd91e72194e48fcf1ac16302d686bf8e3a71434bdbc067a6efd3fdc7`,
build `ca4c8d30` (the v1.9.0 tag).

| Piece | Flags and objects |
|---|---|
| IAM service | `versitygw --port :7070 --health /healthz --region us-east-1 iam --dir /iam --private-ports /sock/iam.sock --oidc-discovery-url 'https://sts-bridge.kind.invalid=https://vsts-bridge:8443/.well-known/openid-configuration'`, root keys in `ROOT_ACCESS_KEY` / `ROOT_SECRET_KEY`. It serves IAM and STS (`AssumeRoleWithWebIdentity`, unauthenticated as STS defines it) on `:7070`, and a private API for the gateway on the socket. |
| S3 gateway | `versitygw --port :8333 --health /healthz --region us-east-1 --access-log - --iam-standalone-endpoint /sock/iam.sock posix /data`, with the same root keys (it signs its private calls with them). For every request signed with a session or IAM-user key it asks the IAM service for the signing key and a policy decision. Root keys stay local. |
| Gateway ↔ IAM link | A unix socket on a shared volume. A TCP private port requires mTLS: `--private-cert`, `--private-cert-key` and `--private-client-ca` on the IAM side, and `--iam-standalone-client-cert`, `--iam-standalone-client-cert-key` and `--iam-standalone-server-ca` on the gateway. |
| Issuer | HTTPS is required. The discovery document and JWKS are served by the bridge, with a leaf certificate for the dialed name signed by a test CA that is sent in the chain. |
| OIDC provider | `CreateOpenIDConnectProvider`: Url `https://sts-bridge.kind.invalid`, ClientIDList `kind-sts-bridge` (plus `kind-other-client` for check T3), ThumbprintList = the test CA's SHA-1. versitygw pins the chain's top certificate, then verifies the leaf against it for the dialed host name. |
| Vend role | `CreateRole kind-storage-vend`, MaxSessionDuration 3600. `PutRolePolicy vend`: Get, Put and AbortMultipartUpload on `snapshots/*` and `templates/*`, ListBucket on both buckets (the RustFS bake-off's user policy). |
| Trust policy | Federated = `arn:aws:iam::000000000000:oidc-provider/sts-bridge.kind.invalid`, Action `sts:AssumeRoleWithWebIdentity`, StringEquals `sts-bridge.kind.invalid:sub` = `kind-storage-vend` and `sts-bridge.kind.invalid:aud` = `kind-sts-bridge` |
| Decoy role | `kind-storage-other`: the same policy, trusting sub `someone-else` |

Not needed: `--oidc-allow-private-endpoints` and `--oidc-allow-insecure-transport`.
The token's issuer is a name that never resolves, and `--oidc-discovery-url`
moves only the fetch to the bridge's container, with TLS still verified
against the pinned CA. versitygw's account is fixed at `000000000000`, so the
role ARN is `arn:aws:iam::000000000000:role/kind-storage-vend`, the same ARN
the bake-off used. The vend identity's keys exist only in the bridge; they are
not a versitygw user, so they open nothing at the S3 endpoint (C40, C41).

`stack.sh` does all of this; `results/s1-setup.md` has the record.

## Results

All runs were on 2026-10-16, against one Docker host. The gateway had the Kind
chart's limits (1 CPU, 4 GiB) and the IAM service had 1 CPU and 1 GiB. Raw
tables are in `results/`.

### C: criterion C (`results/c-exchange-*.md`)

The bake-off's 47 checks, with SPACE's object-store client doing everything
the bake-off did with it. Sessions came three ways:

| Path | Runs | Result |
|---|---|---|
| `AssumeRoleWithWebIdentity` directly, tokens from the bridge's signer | 4 | 46/47, 47/47, 46/47, 46/47 |
| Bridge, SDK `assume_role` | 1 | 46/47 |
| Bridge, the authority's `AssumeRoleProvider` code (`load_defaults`, `AWS_ENDPOINT_URL_STS`) | 1 | 46/47 |

The statuses were the same on every path:

| Checks | Expected | Observed |
|---|---|---|
| C1 AssumeRole with Policy, DurationSeconds 3600 | session, ~3600 s | 3600 s |
| C2–C6, C10, C18, C19, C24–C29 (in scope: put_file, put_stream, Head, ranged Get, List on the key or prefix, multipart create/abort) | allowed | 200 |
| C7–C9, C30, C31 (List outside the key or prefix) | refused | 403 AccessDenied |
| C11–C13 (Head on another sandbox's object, an extended key, a child key) | refused | 403 |
| C11w–C14w, C20, C32–C35 (small PUTs out of scope or under read scope) | refused | 403 AccessDenied |
| C14 (Get in the other vended bucket) | refused | 403 AccessDenied |
| C15 Delete, C16 Copy from another sandbox | refused | 403 AccessDenied |
| C17, C23 (objects after the refused writes) | unchanged | unchanged, 6 of 6 runs |
| **C21 read-only session, 20 MiB put_file** | **refused (403)** | **Refused in every run, but the client saw 403 in 1 of 6 runs; in 5 it saw a connection reset after 3 attempts (see the verdict).** |
| C22 read-only CreateMultipartUpload | refused | 403 AccessDenied |
| C36 token with its last character changed | refused | 400 InvalidToken |
| C37 a wider session's token with this session's keys ¹ | refused | 400 InvalidToken |
| C38 this session's key and token, signed with the vend secret ¹ | refused | 403 SignatureDoesNotMatch |
| C39 session keys without the token | refused | 403 InvalidAccessKeyId |
| C39b session keys with another session's token | refused | 400 InvalidToken |
| C40 vend keys on S3 (info) | — | 403 InvalidAccessKeyId (RustFS allowed it) |
| C41 vend keys, bucket outside the policy | refused | 403 InvalidAccessKeyId |
| C42/C43 role that names no role / another account (info) | AWS refuses | 403 AccessDenied (RustFS issued a session) |
| C44/C45 malformed / missing RoleArn (info) | AWS refuses | 400 ValidationError (RustFS issued a session) |
| C46 no session Policy (info) | the role's policy | allowed in the vended buckets |

¹ versitygw's session tokens are opaque random strings stored server-side, not
JWTs. The bake-off's C37 (widen the JWT's embedded policy) and C38 (re-sign
the JWT) have nothing to alter, so they are replaced by the nearest forgeries:
borrowing a wider session's token, and signing with the one secret the caller
does know.

### Expiry (`results/c-expiry-*.md`)

| # | Check | web-identity | bridge, authority's client |
|---|---|---|---|
| E.1, E.60 | DurationSeconds 1, 60 (info) | 400 ValidationError | 400 ValidationError |
| E.900 | DurationSeconds 900 (info) | issued, 900 s | issued, 900 s |
| E1 | fresh: HeadObject | 200 | 200 |
| E2 | +840 s, 60 s before expiry | 200 | 200 |
| E3 | +930 s, 30 s past expiry: HeadObject | 403 | 403 |
| E4 | past expiry: GetObject | 403 InvalidAccessKeyId | 403 InvalidAccessKeyId |

Durations below 900 s are refused, as AWS does (RustFS silently raised them to
900). The authority always asks for 3600 s (`STORAGE_SESSION_TTL_SECONDS`).

### Trust, through the bridge's token path (`results/trust.md`, `results/bridge.md`)

Tokens came from the bridge's signer with the bridge's claims, or with one
claim or the key changed, and went to versitygw's `AssumeRoleWithWebIdentity`.

| # | Case | Observed | AWS |
|---|---|---|---|
| T0 | control: the bridge's sub, aud, issuer and key; HeadObject with the session | issued; 200 | — |
| T1 | subject the trust policy does not name | 400 InvalidIdentityToken | 403 AccessDenied |
| T2 | audience not in the provider's ClientIDList | 400 InvalidIdentityToken | 400 InvalidIdentityToken |
| T3 | audience in the ClientIDList, not in the trust policy | 400 InvalidIdentityToken | 403 AccessDenied |
| T4 | issuer with no registered provider | 400 InvalidIdentityToken | 400 InvalidIdentityToken |
| T5 | trusted issuer name, untrusted key, trusted kid | 400 InvalidIdentityToken | 400 |
| T6 | trusted issuer name, untrusted key, its own kid | 400 InvalidIdentityToken | 400 |
| T7 | unsigned (`alg: none`) | 400 InvalidIdentityToken | 400 |
| T8 | expired 10 minutes ago | 400 ExpiredTokenException | 400 ExpiredTokenException |
| T8i | expired 2 minutes ago (info) | issued: 5 minutes of skew allowed | the same |
| T9 | role that does not exist | 403 AccessDenied | 403 |
| T10 | role whose trust names another subject | 400 InvalidIdentityToken | 403 AccessDenied |
| T11 | DurationSeconds 3601 > role max 3600 | 400 ValidationError | 400 ValidationError |
| T12 | DurationSeconds 43201 > STS max | 400 ValidationError | 400 |
| T13 | the role's name in another account | 403 AccessDenied | 403 |
| T14 | the role's name under another path | 403 AccessDenied | 403 |
| T15i | DurationSeconds 899 (info) | 400 ValidationError | 400 |
| B10 | through the bridge: no such role | 403 AccessDenied | 403 |
| B11 | through the bridge: trust names another subject | 403 AccessDenied (bridge maps InvalidIdentityToken) | 403 |
| B12 | through the bridge: DurationSeconds 3601 | 400 ValidationError | 400 |
| B13 | through the bridge: malformed role ARN | 400 ValidationError | 400 |
| B14 | the authority's client: no such role | CredentialsError carrying 403 AccessDenied | the same |

The AWS column is AWS STS's documented or commonly observed answer, for
comparison only: none of these were run against AWS. Every case is refused.
The status differs from AWS in T1, T3 and T10, which the bridge's mapping
hides from the authority (B11).

### Forged credentials (`results/c-exchange-*.md` C36–C39b, `results/trust.md` F)

| # | Credential | Observed |
|---|---|---|
| C36 | token with its last character changed | 400 InvalidToken |
| C37 | a wider session's token with this session's keys | 400 InvalidToken |
| C38 | key and token, signed with the vend secret | 403 SignatureDoesNotMatch |
| C39 | keys without the token | 403 InvalidAccessKeyId |
| C39b | keys with another session's token | 400 InvalidToken |
| F1 | a made-up token of the same length | 400 InvalidToken |
| F2 | secret and token under another session's access key | 400 InvalidToken |
| F3 | access key with one character changed | 403 InvalidAccessKeyId |
| F4 | secret with one character changed | 403 SignatureDoesNotMatch |
| F5 | the genuine session afterwards | 200 |

400 InvalidToken is S3's documented answer for a malformed or invalid token.
Unlike RustFS's 500, it is not retried as a server error.

### B1: mismatched Content-MD5 under a session (`results/b1-content-md5-*.md`)

The session came from the authority's client through the bridge, with the
authority's prefix policy. Writes used the session; reads back and ListParts
used admin keys.

| # | Check | Session | Static admin keys |
|---|---|---|---|
| B1.1 | PutObject, UNSIGNED-PAYLOAD, wrong Content-MD5 | 400 BadDigest | 400 BadDigest |
| B1.2 | the key afterwards | 404 | 404 |
| B1.3 | PutObject, signed payload, wrong Content-MD5 | 400 BadDigest | 400 BadDigest |
| B1.4 | the key afterwards | 404 | 404 |
| B1.5 | overwrite with a wrong Content-MD5 | 400 BadDigest, original kept | the same |
| B1.6 | UploadPart, wrong Content-MD5 | 400 BadDigest | 400 BadDigest |
| B1.7 | ListParts afterwards | part 1 only | part 1 only |
| B1.8 | Complete naming the refused part | 400 EntityTooSmall, no object | the same |

### Burst (`results/d-burst.md`)

The bake-off's 36 × 128 MiB burst (client `put_file` phase, then an SDK phase
with a timed body, and zero-progress sampling on the gateway's veth). There
were three runs of each kind, interleaved, on one stack, writing to
`snapshots`.

| Credentials | Client phase, median (s) | Worst (s) | Slowest PUT, median (s) | Max ACK gap, median (s) | Stalls ≥ 5 s | Failed uploads | Gateway CPU med/max (%) | IAM service CPU med/max (%) |
|---|---|---|---|---|---|---|---|---|
| static (root keys, no IAM call) | 17.11 | 17.32 | 17.11 | 1.73 | 0 | 0 | 100 / 105 | 0 / 0 |
| session (authority's prefix policy) | 17.15 | 17.16 | 17.15 | 1.92 | 0 | 0 | 100 / 111 | 0 / 8 |
| IAM user, static keys, vend policy (info) | 17.29 | 17.39 | 17.29 | 1.81 | 0 | 0 | 100 / 104 | 0 / 9 |

Session / static client-phase medians: **1.002**. The longest zero-progress
window with a body still sending was 0.60 s. The gateway is CPU-bound at its
1-CPU limit in every mode; policy evaluation costs the IAM service under 10 %
of a core at peak. The bake-off measured 17.68 s for v1.8.0 on its own.

The Docker host had 23.5 GB free before the burst and 23.5 GB after
(`docker system df`: 15.2 GB images, 13.3 GB build cache), so no cleanup was
needed at any point.

### Resilience (`results/resilience.txt`, not in the rule)

| Event | Session GetObject | Root-key GetObject | AssumeRole through the bridge |
|---|---|---|---|
| IAM service stopped | 500 (gateway: `derive-signing-key` unreachable) | 200 | 503 ServiceUnavailable |
| IAM service restarted | 200, same session | 200 | 200 |
| Gateway restarted | 200, same session | 200 | — |
| Bridge stopped | 200 | — | connection refused |
| Bridge restarted | 200 | — | 200 |

As with RustFS, an IAM-service outage is an STS outage. It also breaks
session-signed S3 traffic, which the SDK retries as a 5xx, while root keys keep
working.

## The bridge

`bridge/` (Go, standard library only). The request path:

1. Reads the body (64 KiB cap) of `POST /` and **verifies SigV4** against
   `BRIDGE_IDENTITIES` (`ACCESS_KEY:SECRET:SUBJECT`, from the environment).
2. Requires `Action=AssumeRole`, `Version=2011-06-15` and parameters in the
   signed form body. It forwards only `RoleArn`, `RoleSessionName`, `Policy`
   and `DurationSeconds`; anything else (ExternalId, Tags, PolicyArns, MFA) is
   refused, not silently dropped (B9).
3. **Mints** an RS256 JWT: `iss` = the registered issuer, `sub` = the verified
   identity's subject, `aud` = the bridge's client ID, `iat`/`nbf` now,
   `exp` +5 min, a random `jti`.
4. Calls versitygw's `AssumeRoleWithWebIdentity` with that token and the same
   four parameters.
5. Returns `AssumeRoleResponse` XML (Credentials, AssumedRoleUser,
   PackedPolicySize, RequestId), or STS's `ErrorResponse`. versitygw's status
   and code pass through, except that `InvalidIdentityToken` and
   `ExpiredTokenException` become 403 AccessDenied (to AssumeRole's caller,
   the role did not trust it) and `IDPCommunicationError` becomes 503.
6. Serves `/.well-known/openid-configuration` and `/jwks.json` over HTTPS on a
   second port, for versitygw.

**What its SigV4 verification covers** (`results/bridge.md`, B2–B8):

| Fault | Answer |
|---|---|
| no Authorization header (B2) | 403 MissingAuthenticationToken |
| wrong secret (B3) | 403 SignatureDoesNotMatch |
| unknown access key (B4) | 403 InvalidClientTokenId |
| body changed after signing (B5) | 403 SignatureDoesNotMatch: the canonical request hashes the body actually received |
| X-Amz-Date more than 15 minutes off (B6) | 403 SignatureDoesNotMatch "Signature expired". With retries on, the SDK reads the skew from the refusal's Date header and re-signs (B6i), as it does against AWS. |
| a session token next to the vend keys (B7), a vended session's own keys (B8) | 403 InvalidClientTokenId: no role chaining |

It also checks the credential scope (date matching X-Amz-Date, service `sts`,
`aws4_request`), that `host` and `x-amz-date` are signed, the method, path,
query, every signed header, and `x-amz-content-sha256` when sent. The
comparison is constant-time.

**What it does not cover:** presigned (query-string) requests, SigV4A,
chunked payload signing, replay of a captured request within the 15-minute
window (AWS accepts that too), and any IAM permission on the caller: an
identity in `BRIDGE_IDENTITIES` may ask for any role, and the role's trust
policy (which names the bridge's subject) is the only authorization. It never
checks the region in the scope. It is plaintext HTTP. Anyone holding its
signing key can mint tokens versitygw trusts, so that key is as sensitive as
the vend secret.

**Size:** 744 lines including comments (651 code), 10 MB static binary.
The request path (`sigv4.go` + `sts.go`) is 425 lines; `oidc.go` (issuer,
minting, test keys) and `main.go` are the rest. Round trip through the bridge
and versitygw: p50 8 ms, max 27 ms (the first call, which fetched the JWKS),
over 32 calls (`results/s3-bridge-log.md`).

## How it would sit in Kind's charts

Exchange mode on the bake-off branch points the authority at the store's STS.
With versitygw, only the STS endpoint and the vend identity's owner change:

- **space-object-store** (versitygw, as today): a second container in the pod
  runs `versitygw iam`, with `--dir` on a subdirectory of the store's hostPath
  (condition 3) and `--private-ports` on a socket in an `emptyDir` shared with
  the gateway container. The gateway gains `--iam-standalone-endpoint` pointing
  at that socket. `--oidc-discovery-url https://sts-bridge.kind.invalid=https://sts-bridge:8443/.well-known/openid-configuration`
  points the IAM service at the bridge Service. The IAM port (7070) need only
  be reachable from the bridge and the bucket-init Job.
- **bucket-init Job**: as today, it creates `snapshots` and `templates` with
  the root keys. In exchange mode it also calls the IAM API, idempotently:
  `CreateOpenIDConnectProvider` (ClientIDList, ThumbprintList = SHA-1 of the
  bridge's CA, computed with `openssl x509 -fingerprint -sha1`), then
  `CreateRole kind-storage-vend` with the trust policy, `--max-session-duration 3600`
  and `PutRolePolicy`.
- **space-kind-sts-bridge** (new, Kind-only, like `space-kind-key-broker`): a
  Deployment, plus a Service `sts-bridge` with ports 8080 (STS, plaintext) and
  8443 (issuer, TLS). A **Secret** `space-kind-sts-bridge` holds
  `BRIDGE_IDENTITIES`, the signing key and the issuer's TLS leaf, key and CA.
  Helm's `genCA`, `genSignedCert` and `genPrivateKey` can render it, with
  `lookup` to keep it stable across upgrades. The CA certificate also goes to
  the bucket-init Job for the thumbprint.
- **space-credmgr-authority-local**: `storageExchange.stsEndpoint` becomes
  `http://sts-bridge:8080`, so `AWS_ENDPOINT_URL_STS` points at the bridge.
  `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` come from the bridge's Secret
  (its vend identity), not from a store user. `SPACE_CREDMGR_S3_VEND_ROLE_ARN`
  is `arn:aws:iam::000000000000:role/kind-storage-vend`. No authority code
  changes.
- **Plaintext hop**: the authority → bridge call is plaintext, as the bake-off's
  authority → RustFS STS call was. `infra/space/kind/AGENTS.md` names the key
  broker as the one plaintext hop, so this needs either an entry there or a
  TLS listener on the bridge from `scripts/internal-rpc-tls.sh`'s CA, with the
  authority trusting that CA. That was not tested here.

What stays different from a deployment: the base credential is a static
key pair verified by the bridge, not a workload identity, and STS shares the
store's failure domain (resilience table).

## What to report upstream to versitygw

Not filed (filing is outside this task's authorization).

1. **An early error response on a request with an unread body closes the
   connection without draining it.** Clients that write the whole body before
   reading (hyper, so aws-sdk-rust) usually see `ECONNRESET` instead of the
   403/404, and retry. It reproduces on v1.8.0 (single gateway) and on v1.9.0
   with the standalone IAM service: a 20 MiB PUT with a wrong secret,
   `results/early-refusals*.md`. Draining a bounded amount (Go's net/http reads
   up to 256 KiB) or lingering before close would let the status through.
2. **Trust-policy refusals answer 400 InvalidIdentityToken where AWS answers
   403 AccessDenied** ("Not authorized to perform sts:AssumeRoleWithWebIdentity"):
   a subject or audience the trust policy's conditions don't name (T1, T3),
   and a role trusting another subject (T10). `EvaluateWebIdentityTrust`'s
   `ConditionFailed` maps to `InvalidIdentityTokenClaims`.
3. **A session-policy denial is worded as an identity-policy denial**: "because
   no identity-based policy allows the s3:PutObject action", for a role whose
   identity policy allows it and whose session policy does not. AWS says "no
   session policy allows".
4. **An unreachable IAM service makes the gateway answer session-signed
   requests with 500 InternalError.** 503 would match the situation, though
   SDKs retry both.

## How to reproduce

From the repository root on a Linux box whose network namespace holds the
Docker bridge, with docker, go, the aws CLI, curl, jq, protoc and a Rust
toolchain:

```bash
cd scratch/versitygw-sts-spike
./stack.sh up
cp ../../Cargo.lock probe/
export CARGO_TARGET_DIR=/var/tmp/versitygw-sts-spike-target
(cd probe && cargo build --release)
P=$CARGO_TARGET_DIR/release/versitygw-sts-spike-probe
export BAKEOFF_SCRATCH=/var/tmp/versitygw-sts-spike-scratch RUST_LOG=error
eval "$(./stack.sh env)"

$P --label web-identity expiry --sts-mode web-identity > results/c-expiry-web-identity.md &
$P --label "bridge, authority client" expiry --sts-mode provider > results/c-expiry-bridge-authority-client.md &
$P --label web-identity exchange --sts-mode web-identity
$P --label "bridge, SDK client" exchange --sts-mode bridge
$P --label "bridge, authority client" exchange --sts-mode provider
$P --label versitygw trust
$P --label "versitygw + bridge" bridge-auth --sts-mode provider
$P --label versitygw early --repeats 20
eval "$($P session --sts-mode provider --prefix spike/b1)"
$P --label session digest --bucket snapshots --key-prefix spike/b1
unset BAKEOFF_WRITER_ACCESS_KEY BAKEOFF_WRITER_SECRET_KEY BAKEOFF_WRITER_SESSION_TOKEN
PROBE=$P ./burst.sh
wait                                  # the expiry runs take ~16 min
./stack.sh down
```

`README.md` describes each tool. The v1.8.0 control in
`results/early-refusals-v1.8.0-static.md` is the same `early` run against
`ghcr.io/versity/versitygw:v1.8.0@sha256:30292fc2…a2499` started as Kind's
chart starts it, with `SPIKE_MINT=false` so the session case is skipped.
