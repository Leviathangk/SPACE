
### Bridge — versitygw + bridge

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| B0 | authority's AssumeRoleProvider (load_defaults, AWS_ENDPOINT_URL_STS = bridge) | credentials, expiry ~3600 s | allowed (session for 3600 s) | PASS |
| B0a | its session: HeadObject on the policy's object | allowed | allowed (Some(ObjectMeta { size: 11, etag: Some("\"778a56d4e6330f4ca1573383868cb16c\"") })) | PASS |
| B0b | its session: HeadObject on another object | refused | refused (S3 head_object failed: HTTP 403) | PASS |
| B0c | its session: PutObject under a read policy | refused | refused (S3 put_object failed: HTTP 403 AccessDenied) | PASS |
| B1 | SDK assume_role (Policy, DurationSeconds 3600), vend keys | credentials | allowed (session for 3600 s) | PASS |
| B2 | SigV4: unsigned (Authorization removed after signing) | refused (403) | refused (HTTP 403 MissingAuthenticationToken) | PASS |
| B3 | SigV4: signed with a wrong secret | refused (403) | refused (HTTP 403 SignatureDoesNotMatch) | PASS |
| B4 | SigV4: an access key the bridge does not know | refused (403) | refused (HTTP 403 InvalidClientTokenId) | PASS |
| B5 | SigV4: Policy changed after signing (GetObject to PutObject, same length) | refused (403) | refused (HTTP 403 SignatureDoesNotMatch) | PASS |
| B6 | SigV4: signed 20 minutes ago | refused (403) | refused (HTTP 403 SignatureDoesNotMatch) | PASS |
| B7 | SigV4: vend keys with a session token | refused (403) | refused (HTTP 403 InvalidClientTokenId) | PASS |
| B6i | signed 20 minutes ago, SDK retries on (its default) | info | issued: the SDK corrected its clock from the refusal and re-signed | info |
| B8 | SigV4: a vended session's own credentials (role chaining) | refused (403) | refused (HTTP 403 InvalidClientTokenId) | PASS |
| B9 | an AssumeRole parameter the bridge does not forward (ExternalId) | refused | refused (HTTP 400 InvalidParameterValue) | PASS |
| B10 | trust through the bridge: a role that does not exist | refused | refused (HTTP 403 AccessDenied) | PASS |
| B11 | trust through the bridge: a role whose trust policy names another subject | refused | refused (HTTP 403 AccessDenied) | PASS |
| B12 | trust through the bridge: DurationSeconds 3601, above the role's maximum | refused | refused (HTTP 400 ValidationError) | PASS |
| B13 | trust through the bridge: a malformed role ARN | refused | refused (HTTP 400 ValidationError) | PASS |
| B14 | authority's client: a role that does not exist | refused (CredentialsError) | refused (provider error: HTTP 403 AccessDenied) | PASS |

18 passed, 0 failed, 1 informational

