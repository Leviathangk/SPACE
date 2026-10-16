Variant: RUSTFS_PUT_LARGE_FOREGROUND_ADMISSION_LIMIT=64 (admit 64 concurrent large writes instead of RustFS 1.0.1's derived limit of at most 32), Kind's limits (1 CPU, 4 GiB), a fresh store per burst.

### D. Start-up and performance — rustfs

Image `rustfs/rustfs:1.0.1@sha256:1803faef57627e2d9c2e7d89d655d712ddded5389040054987163043fecb6a3c`: 111 MB compressed (linux/amd64 layers), 404 MB unpacked.

| Measure | Result |
|---|---|
| start → ready, fresh data, run 1 | health 0.619 s, first S3 answer 0.627 s |
| start → ready, restart on data | health 0.160 s, first S3 answer 0.167 s |
| first write to a new bucket | median 11.6 ms over 5 new buckets |
| idle memory (MiB) | median 20.1, max 20.6 |
| idle CPU (% of one core) | median 0.0, max 0.1 |
| memory during the bursts (MiB) | median 620.0, max 1270.8 |
| CPU during the bursts (% of one core) | median 99.6, max 107.0 |
| burst phase (client put_file), median / worst of 3 | 22.26 s / 23.06 s |
| slowest PUT, median / worst | 22.26 s / 23.06 s |
| longest ACK gap (body sent → response), median / worst | 10.40 s / 12.23 s |
| zero-progress stalls >= 5 s while a body is sending | 0 |
| failed uploads | 0 of 216 |

| D1 | run 1: 36 x 128 MiB at once |  | client phase 23.06 s, slowest PUT 23.06 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 8.41 s; SDK phase 22.78 s, max ACK gap 12.23 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
| D1 | run 1: 36 x 128 MiB at once |  | client phase 22.00 s, slowest PUT 22.00 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 7.41 s; SDK phase 23.71 s, max ACK gap 10.40 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
| D1 | run 1: 36 x 128 MiB at once |  | client phase 22.26 s, slowest PUT 22.26 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 7.71 s; SDK phase 22.87 s, max ACK gap 9.72 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
