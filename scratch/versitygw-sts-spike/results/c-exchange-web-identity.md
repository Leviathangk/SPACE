
### C. Exchange requirements — versitygw web-identity

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| C1 | AssumeRole (Policy, DurationSeconds 3600) signed by the vend identity | credentials, expiry ~3600 s | session expires in 3600 s | PASS |
| C2 | exact-object RW: put_file multipart (Create/UploadPart/Complete under s3:PutObject) | allowed | allowed (20971520 bytes) | PASS |
| C3 | exact-object RW: put_stream 12 MiB in 5 MiB parts | allowed | Ok(12582912) | PASS |
| C4 | exact-object RW: HeadObject (under s3:GetObject) | allowed | allowed (Some(ObjectMeta { size: 20971520, etag: Some("\"c2b094b47530c7ccdb082150dbcc2dd0\"") })) | PASS |
| C5 | exact-object RW: get_file (ranged If-Match GETs) | allowed, identical | allowed (20971520 bytes), identical = true | PASS |
| C6 | ListObjectsV2 prefix = the object key (StringEquals s3:prefix) | allowed | allowed (2 keys) | PASS |
| C7 | ListObjectsV2 prefix = the sandbox's directory | refused | refused (HTTP 403 AccessDenied) | PASS |
| C8 | ListObjectsV2 with no prefix | refused | refused (HTTP 403 AccessDenied) | PASS |
| C9 | ListObjectsV2 prefix = another sandbox's key | refused | refused (HTTP 403 AccessDenied) | PASS |
| C10 | CreateMultipartUpload + AbortMultipartUpload on the object | allowed | allowed (created, part uploaded, aborted) | PASS |
| C11 | HeadObject on another sandbox's object | refused | refused (S3 head_object failed: HTTP 403) | PASS |
| C11w | PutObject on another sandbox's object | refused | refused (S3 put_object failed: HTTP 403 AccessDenied) | PASS |
| C12 | HeadObject on a key extending the object's name | refused | refused (S3 head_object failed: HTTP 403) | PASS |
| C12w | PutObject on a key extending the object's name | refused | refused (S3 put_object failed: HTTP 403 AccessDenied) | PASS |
| C13 | HeadObject on a key under the object's name | refused | refused (S3 head_object failed: HTTP 403) | PASS |
| C13w | PutObject on a key under the object's name | refused | refused (S3 put_object failed: HTTP 403 AccessDenied) | PASS |
| C14 | GetObject on the same key in the other vended bucket | refused | refused (object-store access denied: HTTP 403 AccessDenied) | PASS |
| C14w | PutObject on the same key in the other vended bucket | refused | refused (S3 put_object failed: HTTP 403 AccessDenied) | PASS |
| C15 | DeleteObject on the object (not granted) | refused | refused (HTTP 403 AccessDenied) | PASS |
| C16 | CopyObject from another sandbox's object onto the object | refused | refused (HTTP 403 AccessDenied) | PASS |
| C17 | other sandbox's object afterwards | unchanged | unchanged = true | PASS |
| C18 | read-only: HeadObject | allowed | allowed (Some(ObjectMeta { size: 20971520, etag: Some("\"c2b094b47530c7ccdb082150dbcc2dd0\"") })) | PASS |
| C19 | read-only: GetObject | allowed | allowed (20971520 bytes) | PASS |
| C20 | read-only: PutObject | refused | refused (S3 put_object failed: HTTP 403 AccessDenied) | PASS |
| C21 | read-only: put_file multipart | refused | refused (S3 put_object failed: dispatch failure: io error: client error (SendRequest): error writing a body to connection: Connection reset by peer (os error 104) (Dispa) | FAIL |
| C22 | read-only: CreateMultipartUpload | refused | refused (create: HTTP 403 AccessDenied) | PASS |
| C23 | object after the refused writes | unchanged | unchanged = true | PASS |
| C24.0 | prefix RW: PutObject <prefix>/memory | allowed | allowed (stored) | PASS |
| C25.0 | prefix RW: HeadObject on it | allowed | allowed (Some(ObjectMeta { size: 1000, etag: Some("\"411d56f0cadbb2ec58784594661a1b67\"") })) | PASS |
| C24.1 | prefix RW: PutObject <prefix>/nested/rootfs | allowed | allowed (stored) | PASS |
| C25.1 | prefix RW: HeadObject on it | allowed | allowed (Some(ObjectMeta { size: 1000, etag: Some("\"72d774aafe1f5e9cbaa5987da0763931\"") })) | PASS |
| C26 | prefix RW: put_stream multipart under the prefix | allowed | Ok(11534336) | PASS |
| C27 | prefix: ListObjectsV2 prefix=<prefix> | allowed | allowed (4 keys) | PASS |
| C28 | prefix: ListObjectsV2 prefix=<prefix>/ | allowed | allowed (3 keys) | PASS |
| C29 | prefix: ListObjectsV2 prefix=<prefix>/nested | allowed | allowed (1 keys) | PASS |
| C30 | prefix: ListObjectsV2 prefix=<prefix>x | refused | refused (HTTP 403 AccessDenied) | PASS |
| C31 | prefix: ListObjectsV2 prefix=suspend_snapshots/07/18df08e5-aaaa-4aaa-8aaa-aaaaaaaaaaaa/11111111-2222-4333-8444-555555555555 | refused | refused (HTTP 403 AccessDenied) | PASS |
| C32 | prefix: PutObject on the prefix itself as a key | refused | refused (S3 put_object failed: HTTP 403 AccessDenied) | PASS |
| C33 | prefix: PutObject on a sibling sharing the prefix string | refused | refused (S3 put_object failed: HTTP 403 AccessDenied) | PASS |
| C34 | prefix: PutObject on another sandbox's object | refused | refused (S3 put_object failed: HTTP 403 AccessDenied) | PASS |
| C35 | prefix: PutObject under the same prefix in the other bucket | refused | refused (S3 put_object failed: HTTP 403 AccessDenied) | PASS |
| C36 | session token with its last character changed | refused (AWS: 403) | refused (S3 head_object failed: HTTP 400) | PASS |
| C37 | opaque token: a wider session's token (no session Policy) with this session's keys | refused (AWS: 403) | refused (S3 head_object failed: HTTP 400) | PASS |
| C38 | opaque token: this session's key and token, signed with the vend identity's own secret | refused (AWS: 403) | refused (S3 head_object failed: HTTP 403) | PASS |
| C39 | session access key and secret without the token | refused (AWS: 403) | refused (S3 head_object failed: HTTP 403) | PASS |
| C39b | session keys with another session's token | refused (AWS: 403) | refused (S3 head_object failed: HTTP 400) | PASS |
| C40 | vend identity's own keys (no session): HeadObject in the vended bucket | info | refused (S3 head_object failed: HTTP 403) | info |
| C41 | vend identity's own keys: PutObject in a bucket outside its policy | refused | refused (S3 put_object failed: HTTP 403 InvalidAccessKeyId) | PASS |
| C42 | role validation: AssumeRole with a role ARN that names no role | AWS refuses; recorded only | refused (HTTP 403 AccessDenied) | info |
| C43 | role validation: AssumeRole with a role ARN in another account | AWS refuses; recorded only | refused (HTTP 403 AccessDenied) | info |
| C44 | role validation: AssumeRole with a malformed role ARN | AWS refuses; recorded only | refused (HTTP 400 ValidationError) | info |
| C45 | role validation: AssumeRole with no RoleArn | AWS refuses; recorded only | refused (HTTP 400 ValidationError) | info |
| C46 | AssumeRole without Policy: HeadObject on any vended object | info: parent's policy only | allowed (Some(ObjectMeta { size: 65536, etag: Some("\"fde8721dc3b1017ff6ea2b844e05bf21\"") })) | info |

46 passed, 1 failed, 6 informational

