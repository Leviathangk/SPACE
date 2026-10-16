| Case | Expected | Observed | Holds |
|---|---|---|---|
| [space-local-worker2] read exchange: subject A, A's memfile (exact object) | issued | issued | yes |
| [space-local-worker2] read session: HeadObject A's memfile | 200 | 200 | yes |
| [space-local-worker2] read session: GetObject A's memfile, Range 0-0 | 200 | 200 | yes |
| [space-local-worker2] read session: ListBucket prefix = A's memfile | 200 | 200 | yes |
| [space-local-worker2] read session: HeadObject B's memfile | AccessDenied | AccessDenied (403) | yes |
| [space-local-worker2] read session: GetObject B's memfile | AccessDenied | AccessDenied | yes |
| [space-local-worker2] read session: ListBucket prefix = B's snapshot | AccessDenied | AccessDenied | yes |
| [space-local-worker2] read session: GetObject another of A's files (binfile) | AccessDenied | AccessDenied | yes |
| [space-local-worker2] read session: PutObject A's memfile (write under read-only) | AccessDenied | AccessDenied | yes |
| [space-local-worker2] write exchange: subject A, A's snapshot prefix | issued on A's node; refused elsewhere | refused: Code: PermissionDenied | (recorded) |
| [space-local-worker2] read exchange: subject A, B's memfile | refused | refused: Code: PermissionDenied | yes |
| [space-local-worker] read exchange: subject A, A's memfile (exact object) | issued | issued | yes |
| [space-local-worker] read session: HeadObject A's memfile | 200 | 200 | yes |
| [space-local-worker] read session: GetObject A's memfile, Range 0-0 | 200 | 200 | yes |
| [space-local-worker] read session: ListBucket prefix = A's memfile | 200 | 200 | yes |
| [space-local-worker] read session: HeadObject B's memfile | AccessDenied | AccessDenied (403) | yes |
| [space-local-worker] read session: GetObject B's memfile | AccessDenied | AccessDenied | yes |
| [space-local-worker] read session: ListBucket prefix = B's snapshot | AccessDenied | AccessDenied | yes |
| [space-local-worker] read session: GetObject another of A's files (binfile) | AccessDenied | AccessDenied | yes |
| [space-local-worker] read session: PutObject A's memfile (write under read-only) | AccessDenied | AccessDenied | yes |
| [space-local-worker] write exchange: subject A, A's snapshot prefix | issued on A's node; refused elsewhere | issued | (recorded) |
| [space-local-worker] write session: PutObject under A's prefix | 200 | 200 | yes |
| [space-local-worker] write session: PutObject under B's prefix | AccessDenied | AccessDenied | yes |
| [space-local-worker] write session: PutObject over B's memfile | AccessDenied | AccessDenied | yes |
| [space-local-worker] write session: CreateMultipartUpload under B's prefix | AccessDenied | AccessDenied | yes |
| [space-local-worker] write session: PutObject in the templates bucket | AccessDenied | AccessDenied | yes |
| [space-local-worker] read exchange: subject A, B's memfile | refused | refused: Code: PermissionDenied | yes |
