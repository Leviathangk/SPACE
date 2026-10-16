# S3. The bridge's own request log over the spike

`docker logs vsts-bridge` after every run in this directory (one line per
AssumeRole; the bridge logs the caller's subject, status, code and time, never
keys or tokens).

| Status | Code | Requests |
|---|---|---|
| 200 | - | 32 |
| 400 | ValidationError (versitygw: duration or ARN) | 10 |
| 400 | InvalidParameterValue (bridge: unforwarded parameter) | 2 |
| 403 | AccessDenied (versitygw: no such role, other account, trust refused) | 10 |
| 403 | SignatureDoesNotMatch (bridge) | 7 |
| 403 | InvalidClientTokenId (bridge) | 6 |
| 403 | MissingAuthenticationToken (bridge) | 2 |
| 503 | ServiceUnavailable (bridge: IAM service stopped) | 1 |

Successful AssumeRole round trip through the bridge, including the token mint
and versitygw's AssumeRoleWithWebIdentity (JWKS cached after the first): p50
8 ms, max 27 ms (the first call, which fetched the discovery document and
JWKS), over 32 calls.

Clients that got usable credentials from it: the authority's
`AssumeRoleProvider` code (aws-config 1.12.0, aws-sdk-sts 1.114.0), the
SDK's `assume_role` call, aws CLI v2 (botocore) and `curl --aws-sigv4`.

Size: 744 lines of Go including comments (611 non-blank, non-comment), standard
library only; a 10 MB static binary. The request path (SigV4 verification and
the STS translation) is `sigv4.go` + `sts.go`, 425 lines; `oidc.go` (issuer,
token minting, test key generation) and `main.go` (flags) are the rest.
