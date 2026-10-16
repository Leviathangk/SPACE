
### C. Session expiry — versitygw via the bridge (authority client)

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| E.1 | AssumeRole DurationSeconds=1 | info | refused (provider error: HTTP 400 ValidationError) | info |
| E.60 | AssumeRole DurationSeconds=60 | info | refused (provider error: HTTP 400 ValidationError) | info |
| E.900 | AssumeRole DurationSeconds=900 | info | issued, expires in 900 s | info |
| E1 | fresh session (requested 900 s, expires in 900 s): HeadObject | allowed | allowed (Some(ObjectMeta { size: 12, etag: Some("\"4a9b6e473e010d90958e829738a316d1\"") })) | PASS |
| E2 | ~60 s before expiry (+840 s) | allowed | allowed (Some(ObjectMeta { size: 12, etag: Some("\"4a9b6e473e010d90958e829738a316d1\"") })) | PASS |
| E3 | 30 s past expiry (+930 s) | refused | refused (S3 head_object failed: HTTP 403) | PASS |
| E4 | GetObject past expiry | refused | refused (object-store access denied: HTTP 403 InvalidAccessKeyId) | PASS |

4 passed, 0 failed, 3 informational

