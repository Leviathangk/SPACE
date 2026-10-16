
### D. Upload burst — rustfs-4cpu

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| D1 | run 1: 36 x 128 MiB at once |  | client phase 10.52 s, slowest PUT 10.52 s, 12 failed attempts, 4 failed uploads; SDK phase 11.36 s, max ACK gap 8.43 s, 4 failed uploads; longest zero-progress window 3.10 s (0 >= 5 s); first error: S3 put_object failed: HTTP 503 SlowDown | info |
| D2 | run 2: 36 x 128 MiB at once |  | client phase 9.01 s, slowest PUT 9.01 s, 11 failed attempts, 3 failed uploads; SDK phase 11.35 s, max ACK gap 9.57 s, 4 failed uploads; longest zero-progress window 3.10 s (0 >= 5 s); first error: S3 put_object failed: HTTP 503 SlowDown | info |
| D3 | run 3: 36 x 128 MiB at once |  | client phase 10.30 s, slowest PUT 10.30 s, 10 failed attempts, 2 failed uploads; SDK phase 10.47 s, max ACK gap 6.55 s, 28 failed uploads; longest zero-progress window 1.50 s (0 >= 5 s); first error: S3 put_object failed: HTTP 503 SlowDown | info |
| Dm | median / worst over the runs |  | client phase 10.30 s / 10.52 s; slowest PUT 10.30 s / 10.52 s; SDK phase 11.35 s / 11.36 s; max ACK gap 8.43 s / 9.57 s; failed attempts 33; failed uploads 45 | info |
| Ds | zero-progress windows >= 5 s | none | 0 (longest window 3.10 s) | PASS |
| Df | failed uploads | 0 | 45 | FAIL |

1 passed, 1 failed, 4 informational

BURST_SUMMARY label=rustfs-4cpu phase_median_s=10.299 phase_worst_s=10.524 slowest_put_median_s=10.299 slowest_put_worst_s=10.524 ack_gap_median_s=8.428 ack_gap_worst_s=9.571 stalls=0 failures=45
