### D. Start-up and performance — versitygw

Image `ghcr.io/versity/versitygw:v1.8.0@sha256:30292fc2eeacc67a36993b01f7a7a5e3361a19cced0e80c1d71cfa2a4b0a2499`: 31 MB compressed (linux/amd64 layers), 101 MB unpacked.

| Measure | Result |
|---|---|
| start → ready, fresh data, run 1 | health 0.381 s, first S3 answer 0.388 s |
| start → ready, fresh data, run 2 | health 0.173 s, first S3 answer 0.180 s |
| start → ready, fresh data, run 3 | health 0.359 s, first S3 answer 0.366 s |
| start → ready, restart on data | health 0.320 s, first S3 answer 0.327 s |
| first write to a new bucket | median 0.7 ms over 5 new buckets |
| idle memory (MiB) | median 8.8, max 8.8 |
| idle CPU (% of one core) | median 0.0, max 0.0 |
| memory during the bursts (MiB) | median 285.3, max 341.1 |
| CPU during the bursts (% of one core) | median 100.8, max 108.1 |
| burst phase (client put_file), median / worst of 3 | 17.68 s / 17.69 s |
| slowest PUT, median / worst | 17.68 s / 17.69 s |
| longest ACK gap (body sent → response), median / worst | 1.95 s / 2.04 s |
| zero-progress stalls >= 5 s while a body is sending | 0 |
| failed uploads | 0 of 216 |

| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.68 s, slowest PUT 17.68 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.31 s; SDK phase 17.50 s, max ACK gap 1.95 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.47 s, slowest PUT 17.47 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.33 s; SDK phase 17.30 s, max ACK gap 2.04 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.69 s, slowest PUT 17.69 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.43 s; SDK phase 17.70 s, max ACK gap 1.76 s, 0 failed uploads, longest zero-progress window with a body still sending 0.20 s (0 >= 5 s) | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.20 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
