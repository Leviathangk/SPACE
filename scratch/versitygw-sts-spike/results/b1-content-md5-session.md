
### B1. Mismatched Content-MD5 — versitygw, session credentials (authority client via the bridge)

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| B1.1 | PutObject (UNSIGNED-PAYLOAD) with a mismatched Content-MD5 | HTTP 400 BadDigest | HTTP 400 BadDigest | PASS |
| B1.2 | the key afterwards | HTTP 404 (nothing stored) | HTTP 404 NotFound | PASS |
| B1.3 | PutObject (signed payload) with a mismatched Content-MD5 | HTTP 400 BadDigest | HTTP 400 BadDigest | PASS |
| B1.4 | the key afterwards | HTTP 404 | HTTP 404 NotFound | PASS |
| B1.5 | overwrite an existing object with a mismatched Content-MD5 | HTTP 400; original object unchanged | first put Ok(()); overwrite HTTP 400 BadDigest; object = original: true | PASS |
| B1.6 | UploadPart (UNSIGNED-PAYLOAD) with a mismatched Content-MD5 | HTTP 400 BadDigest | HTTP 400 BadDigest | PASS |
| B1.7 | ListParts after the refused part | only part 1 | [1] | PASS |
| B1.8 | CompleteMultipartUpload naming the refused part | HTTP 400 InvalidPart; no object | HTTP 400 EntityTooSmall; object absent = true | PASS |

8 passed, 0 failed, 0 informational

