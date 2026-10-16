### D. Start-up and performance — rustfs

Image `rustfs/rustfs:1.0.1@sha256:1803faef57627e2d9c2e7d89d655d712ddded5389040054987163043fecb6a3c`: 111 MB compressed (linux/amd64 layers), 404 MB unpacked.

| Measure | Result |
|---|---|
| start → ready, fresh data, run 1 | health 0.517 s, first S3 answer 0.524 s |
| start → ready, fresh data, run 2 | health 0.254 s, first S3 answer 0.260 s |
| start → ready, fresh data, run 3 | health 0.454 s, first S3 answer 0.461 s |
| start → ready, restart on data | health 0.356 s, first S3 answer 0.363 s |
| first write to a new bucket | median 11.1 ms over 5 new buckets |
| idle memory (MiB) | median 19.7, max 19.7 |
| idle CPU (% of one core) | median 0.0, max 0.1 |
| memory during the bursts (MiB) | median 409.3, max 933.1 |
| CPU during the bursts (% of one core) | median 99.8, max 106.5 |
| burst phase (client put_file), median / worst of 3 | 14.99 s / 20.19 s |
| slowest PUT, median / worst | 14.99 s / 20.19 s |
| longest ACK gap (body sent → response), median / worst | 9.13 s / 9.63 s |
| zero-progress stalls >= 5 s while a body is sending | 0 |
| failed uploads | 24 of 216 |

| D1 | run 1: 36 x 128 MiB at once |  | client phase 14.99 s, slowest PUT 14.99 s, 12 failed attempts (0 stalled-stream), 4 failed uploads, longest no-ingress window 2.10 s; SDK phase 20.11 s, max ACK gap 9.63 s, 4 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s); first error: S3 put_object failed: HTTP 503 SlowDown | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 8 | FAIL |
| D1 | run 1: 36 x 128 MiB at once |  | client phase 20.19 s, slowest PUT 20.19 s, 12 failed attempts (0 stalled-stream), 4 failed uploads, longest no-ingress window 7.11 s; SDK phase 20.30 s, max ACK gap 8.29 s, 4 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s); first error: S3 put_object failed: HTTP 503 SlowDown | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 8 | FAIL |
| D1 | run 1: 36 x 128 MiB at once |  | client phase 14.81 s, slowest PUT 14.80 s, 12 failed attempts (0 stalled-stream), 4 failed uploads, longest no-ingress window 2.10 s; SDK phase 20.35 s, max ACK gap 9.13 s, 4 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s); first error: S3 put_object failed: HTTP 503 SlowDown | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 8 | FAIL |
