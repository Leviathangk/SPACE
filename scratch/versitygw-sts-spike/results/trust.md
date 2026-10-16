
### Trust and forged credentials — versitygw

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| T0 | control: the bridge's sub, aud and issuer, its signing key | issued | allowed (session for 3600 s) | PASS |
| T0s | control session: HeadObject on its object | allowed | allowed (Some(ObjectMeta { size: 11, etag: Some("\"778a56d4e6330f4ca1573383868cb16c\"") })) | PASS |
| T1 | token: a subject the trust policy does not name | refused | refused (HTTP 400 InvalidIdentityToken) | PASS |
| T2 | token: an audience not in the provider's ClientIDList | refused | refused (HTTP 400 InvalidIdentityToken) | PASS |
| T3 | token: an audience in the ClientIDList that the trust policy does not name | refused | refused (HTTP 400 InvalidIdentityToken) | PASS |
| T4 | token: an issuer with no registered OIDC provider | refused | refused (HTTP 400 InvalidIdentityToken) | PASS |
| T5 | token: the trusted issuer's name, signed by an untrusted key under the trusted kid | refused | refused (HTTP 400 InvalidIdentityToken) | PASS |
| T6 | token: the trusted issuer's name, signed by an untrusted key with its own kid | refused | refused (HTTP 400 InvalidIdentityToken) | PASS |
| T7 | token: an unsigned token (alg none) | refused | refused (HTTP 400 InvalidIdentityToken) | PASS |
| T8 | token: a token that expired 10 minutes ago | refused | refused (HTTP 400 ExpiredTokenException) | PASS |
| T8i | token: expired 2 minutes ago (versitygw allows 5 minutes of skew, as AWS does) | info | allowed (session for 3600 s) | info |
| T9 | role: a role that does not exist | refused | refused (HTTP 403 AccessDenied) | PASS |
| T10 | role: a role whose trust policy names another subject | refused | refused (HTTP 400 InvalidIdentityToken) | PASS |
| T11 | role: DurationSeconds 3601, above the role's MaxSessionDuration 3600 | refused | refused (HTTP 400 ValidationError) | PASS |
| T12 | role: DurationSeconds 43201, above STS's maximum | refused | refused (HTTP 400 ValidationError) | PASS |
| T13 | role: the vend role's name in another account | refused | refused (HTTP 403 AccessDenied) | PASS |
| T14 | role: the vend role's name under another path | refused | refused (HTTP 403 AccessDenied) | PASS |
| T15i | role: DurationSeconds 899, below STS's minimum | info (AWS refuses) | refused (HTTP 400 ValidationError) | info |
| F1 | forged: a made-up token of the same length with this session's keys | refused (AWS: 403) | refused (S3 head_object failed: HTTP 400) | PASS |
| F2 | forged: this session's secret and token under another session's access key | refused (AWS: 403) | refused (S3 head_object failed: HTTP 400) | PASS |
| F3 | forged: this session's access key with one character changed | refused (AWS: 403) | refused (S3 head_object failed: HTTP 403) | PASS |
| F4 | forged: this session's secret with one character changed | refused (AWS: 403) | refused (S3 head_object failed: HTTP 403) | PASS |
| F5 | the genuine session afterwards | allowed | allowed (Some(ObjectMeta { size: 11, etag: Some("\"778a56d4e6330f4ca1573383868cb16c\"") })) | PASS |
| R1 | 20 MiB put_file refused early: admin keys, a bucket that does not exist | info | refused (S3 put_object failed: HTTP 404 NoSuchBucket); attempts:  | info |
| R2 | 20 MiB put_file refused early: admin key, wrong secret | info | refused (S3 put_object failed: HTTP 403 SignatureDoesNotMatch); attempts:  | info |
| R3 | 20 MiB put_file refused early: read-only session, its own object | info | refused (S3 put_object failed: HTTP 403 AccessDenied); attempts: PutObject[vm_state] #1 io  after 3 ms (retry=true); PutObject[vm_state] #2 http_4xx HTTP 403 AccessDenied after 3 ms (retry=false) | info |

21 passed, 0 failed, 5 informational

