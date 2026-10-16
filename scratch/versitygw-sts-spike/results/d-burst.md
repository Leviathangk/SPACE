| Credentials | runs | client phase, median (s) | client phase, worst (s) | slowest PUT, median (s) | max ACK gap, median (s) | stalls >= 5 s | failed uploads | gateway CPU median / max (%) | IAM service CPU median / max (%) |
|---|---|---|---|---|---|---|---|---|---|
| static | 3 | 17.11 | 17.32 | 17.11 | 1.73 | 0 | 0 | 100 / 105 | 0 / 0 |
| session | 3 | 17.15 | 17.16 | 17.15 | 1.92 | 0 | 0 | 100 / 111 | 0 / 8 |
| user | 3 | 17.29 | 17.39 | 17.29 | 1.81 | 0 | 0 | 100 / 104 | 0 / 9 |

session / static, client phase medians: 1.002

### D. Upload burst — static run 1
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.32 s, slowest PUT 17.32 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.38 s; SDK phase 16.62 s, max ACK gap 1.73 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 17.32 s / 17.32 s; slowest PUT 17.32 s / 17.32 s; SDK phase 16.62 s / 16.62 s; max ACK gap 1.73 s / 1.73 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
2 passed, 0 failed, 2 informational
### D. Upload burst — session run 1
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.15 s, slowest PUT 17.15 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.36 s; SDK phase 16.92 s, max ACK gap 1.84 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 17.15 s / 17.15 s; slowest PUT 17.15 s / 17.15 s; SDK phase 16.92 s / 16.92 s; max ACK gap 1.84 s / 1.84 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
2 passed, 0 failed, 2 informational
### D. Upload burst — user run 1
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.29 s, slowest PUT 17.29 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.43 s; SDK phase 16.85 s, max ACK gap 1.41 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 17.29 s / 17.29 s; slowest PUT 17.29 s / 17.29 s; SDK phase 16.85 s / 16.85 s; max ACK gap 1.41 s / 1.41 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
2 passed, 0 failed, 2 informational
### D. Upload burst — static run 2
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.08 s, slowest PUT 17.08 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.42 s; SDK phase 16.66 s, max ACK gap 1.72 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 17.08 s / 17.08 s; slowest PUT 17.08 s / 17.08 s; SDK phase 16.66 s / 16.66 s; max ACK gap 1.72 s / 1.72 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
2 passed, 0 failed, 2 informational
### D. Upload burst — session run 2
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.15 s, slowest PUT 17.15 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.42 s; SDK phase 16.70 s, max ACK gap 1.92 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 17.15 s / 17.15 s; slowest PUT 17.15 s / 17.15 s; SDK phase 16.70 s / 16.70 s; max ACK gap 1.92 s / 1.92 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
2 passed, 0 failed, 2 informational
### D. Upload burst — user run 2
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.39 s, slowest PUT 17.39 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.34 s; SDK phase 16.75 s, max ACK gap 1.86 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 17.39 s / 17.39 s; slowest PUT 17.39 s / 17.39 s; SDK phase 16.75 s / 16.75 s; max ACK gap 1.86 s / 1.86 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
2 passed, 0 failed, 2 informational
### D. Upload burst — static run 3
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.11 s, slowest PUT 17.11 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.34 s; SDK phase 16.64 s, max ACK gap 1.86 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 17.11 s / 17.11 s; slowest PUT 17.11 s / 17.11 s; SDK phase 16.64 s / 16.64 s; max ACK gap 1.86 s / 1.86 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
2 passed, 0 failed, 2 informational
### D. Upload burst — session run 3
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.12 s, slowest PUT 17.12 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.35 s; SDK phase 17.41 s, max ACK gap 1.92 s, 0 failed uploads, longest zero-progress window with a body still sending 0.60 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 17.12 s / 17.12 s; slowest PUT 17.12 s / 17.12 s; SDK phase 17.41 s / 17.41 s; max ACK gap 1.92 s / 1.92 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.60 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
2 passed, 0 failed, 2 informational
### D. Upload burst — user run 3
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.15 s, slowest PUT 17.15 s, 0 failed attempts (0 stalled-stream), 0 failed uploads, longest no-ingress window 0.37 s; SDK phase 16.73 s, max ACK gap 1.81 s, 0 failed uploads, longest zero-progress window with a body still sending 0.00 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 17.15 s / 17.15 s; slowest PUT 17.15 s / 17.15 s; SDK phase 16.73 s / 16.73 s; max ACK gap 1.81 s / 1.81 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress stalls >= 5 s while a body is sending (sampled windows + client stalled-stream detections) | none | 0 (longest window 0.00 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |
2 passed, 0 failed, 2 informational
