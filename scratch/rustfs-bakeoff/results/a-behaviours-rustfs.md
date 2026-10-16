
### A. S3 behaviours SPACE uses — rustfs

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| A1 | put_bytes then HeadObject (client) | size 4096 | size 4096 | PASS |
| A2 | HeadObject on a missing key (client) | None (404) | Ok(None) | PASS |
| A3 | put_file 40 MiB, 8 MiB parts: CreateMultipartUpload, UploadPart x5, Complete (client; UNSIGNED-PAYLOAD + Content-MD5) | Ok(41943040), multipart ETag | Ok(41943040), ETag "85402e75f5a0de549fd2b8f2db8d8d77-5" | PASS |
| A4 | get_file: ranged GETs with If-Match (client) | same SHA-256 as source | Ok(41943040), sha256 match = true | PASS |
| A5 | GET Range bytes=100-199 + If-Match current ETag | 206, the 100 source bytes | Content-Length 100, bytes match = true | PASS |
| A6 | GET Range + If-Match stale ETag | HTTP 412 PreconditionFailed | HTTP 412 PreconditionFailed | PASS |
| A7 | put_stream 20 MiB in 5 MiB parts (client streaming multipart, UNSIGNED-PAYLOAD + Content-MD5) | Ok, same SHA-256 | Ok(20971520), sha256 match = true | PASS |
| A8 | put_stream below one part (client single PUT) | Ok, same SHA-256 | Ok(100000), sha256 match = true | PASS |
| A9 | CreateMultipartUpload, UploadPart, AbortMultipartUpload; upload and object gone | abort 204; ListParts 404 NoSuchUpload; object 404 | abort ok; ListParts HTTP 404 NoSuchUpload; uploads listed 0; object HTTP 404 NotFound | PASS |
| A10 | UploadPartCopy x2 with x-amz-copy-source-range, Complete | copy identical to source | 41943040 bytes, sha256 match = true | PASS |
| A11 | ListObjectsV2 max-keys 10 over 25 keys, continuation tokens | 3 pages, 25 distinct keys in order | 3 pages, 25 keys, 25 distinct, ordered = true | PASS |
| A12 | DeleteObjects (25 keys, one request) | 25 deleted, 0 errors, 0 left | 25 deleted, 0 errors, 0 left | PASS |
| A13 | HeadObject signed with a wrong secret (client) | refused | S3 head_object failed: HTTP 403 | PASS |
| A14 | PutObject signed with a wrong secret | HTTP 403 SignatureDoesNotMatch | HTTP 403 SignatureDoesNotMatch | PASS |
| A15 | GetObject with an unknown access key | refused (403 InvalidAccessKeyId on S3) | HTTP 403 InvalidAccessKeyId | PASS |
| A16 | nothing stored by the refused PUT | HTTP 404 | HTTP 404 NotFound | PASS |
| A17 | PutObject x-amz-content-sha256: UNSIGNED-PAYLOAD + Content-MD5 | 200, same SHA-256 | 200, sha256 match = true | PASS |
| A18 | PutObject STREAMING-UNSIGNED-PAYLOAD-TRAILER body with a CRC32 trailer (not a request SPACE's client sends; it turns trailers off) | info | 200, sha256 match = true | info |

17 passed, 0 failed, 1 informational

