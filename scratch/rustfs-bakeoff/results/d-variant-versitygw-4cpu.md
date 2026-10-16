
### D. Upload burst — versitygw-4cpu

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| D1 | run 1: 36 x 128 MiB at once |  | client phase 8.17 s, slowest PUT 8.17 s, 0 failed attempts, 0 failed uploads; SDK phase 7.65 s, max ACK gap 2.35 s, 0 failed uploads; longest zero-progress window 1.70 s (0 >= 5 s) | info |
| D2 | run 2: 36 x 128 MiB at once |  | client phase 8.48 s, slowest PUT 8.48 s, 0 failed attempts, 0 failed uploads; SDK phase 7.63 s, max ACK gap 2.21 s, 0 failed uploads; longest zero-progress window 0.90 s (0 >= 5 s) | info |
| D3 | run 3: 36 x 128 MiB at once |  | client phase 7.89 s, slowest PUT 7.89 s, 0 failed attempts, 0 failed uploads; SDK phase 7.98 s, max ACK gap 0.94 s, 0 failed uploads; longest zero-progress window 0.90 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 8.17 s / 8.48 s; slowest PUT 8.17 s / 8.48 s; SDK phase 7.65 s / 7.98 s; max ACK gap 2.21 s / 2.35 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress windows >= 5 s | none | 0 (longest window 1.70 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |

2 passed, 0 failed, 4 informational

BURST_SUMMARY label=versitygw-4cpu phase_median_s=8.172 phase_worst_s=8.480 slowest_put_median_s=8.172 slowest_put_worst_s=8.480 ack_gap_median_s=2.210 ack_gap_worst_s=2.348 stalls=0 failures=0
