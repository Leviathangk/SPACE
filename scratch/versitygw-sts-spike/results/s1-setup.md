# S1. versitygw v1.9.0 standalone IAM + test OIDC issuer

`./stack.sh up` on 2026-10-16, image
`ghcr.io/versity/versitygw:v1.9.0@sha256:eb7f786fcd91e72194e48fcf1ac16302d686bf8e3a71434bdbc067a6efd3fdc7`
(build `ca4c8d30`, the v1.9.0 tag).

What it took:

| Piece | Configuration |
|---|---|
| IAM service | `versitygw --port :7070 --health /healthz --region us-east-1 iam --dir /iam --private-ports /sock/iam.sock --oidc-discovery-url 'https://sts-bridge.kind.invalid=https://vsts-bridge:8443/.well-known/openid-configuration'`, root keys in `ROOT_ACCESS_KEY`/`ROOT_SECRET_KEY` |
| S3 gateway | `versitygw --port :8333 --health /healthz --region us-east-1 --access-log - --iam-standalone-endpoint /sock/iam.sock posix /data`, the same root keys (it signs its private calls with them) |
| Gateway ↔ IAM | a unix socket on a shared volume; a TCP private port needs mTLS (`--private-cert`, `--private-cert-key`, `--private-client-ca` on the IAM side, `--iam-standalone-client-cert`, `-key`, `--iam-standalone-server-ca` on the gateway) |
| Issuer | the bridge's HTTPS listener: discovery document and JWKS, leaf certificate for `vsts-bridge` signed by a test CA sent in the chain |
| OIDC provider | `CreateOpenIDConnectProvider` Url `https://sts-bridge.kind.invalid`, ClientIDList `kind-sts-bridge`, ThumbprintList = SHA-1 of the test CA (versitygw pins the chain's top certificate and verifies the leaf against it for the dialed host name) |
| Vend role | `kind-storage-vend`, MaxSessionDuration 3600, inline policy `vend`: Get/Put/AbortMultipartUpload on `snapshots/*`, `templates/*`, ListBucket on both |
| Trust | Federated = the provider ARN, Action `sts:AssumeRoleWithWebIdentity`, StringEquals `<provider>:sub` = `kind-storage-vend` and `<provider>:aud` = `kind-sts-bridge` |
| Decoy role | `kind-storage-other`, same policy, trust naming sub `someone-else` |

Not needed: `--oidc-allow-private-endpoints` and `--oidc-allow-insecure-transport`.
The token's issuer is a name that never resolves; `--oidc-discovery-url` moves only
the fetch to the bridge container, with TLS still verified against the pinned CA.

Smoke (aws CLI v2): `sts assume-role` against the bridge, signed with the vend
identity's keys, returned an `ASIA…` session for
`arn:aws:sts::000000000000:assumed-role/kind-storage-vend/space-storage-exchange`
in 27 ms. With a session policy for `snapshots/a/obj`, the gateway allowed
PutObject and GetObject on it and refused PutObject on `snapshots/a/other` and
`templates/a/obj` with 403 AccessDenied.
