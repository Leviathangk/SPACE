
### D. Upload burst — rustfs-nogate

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| D1 | run 1: 36 x 128 MiB at once |  | client phase 22.53 s, slowest PUT 22.53 s, 0 failed attempts, 0 failed uploads; SDK phase 22.31 s, max ACK gap 11.32 s, 0 failed uploads; longest zero-progress window 8.21 s (2 >= 5 s) | info |
| D2 | run 2: 36 x 128 MiB at once |  | client phase 22.86 s, slowest PUT 22.86 s, 0 failed attempts, 0 failed uploads; SDK phase 22.25 s, max ACK gap 12.12 s, 0 failed uploads; longest zero-progress window 8.31 s (2 >= 5 s) | info |
| D3 | run 3: 36 x 128 MiB at once |  | client phase 22.79 s, slowest PUT 22.79 s, 0 failed attempts, 0 failed uploads; SDK phase 8.15 s, max ACK gap 3.27 s, 36 failed uploads; longest zero-progress window 8.11 s (1 >= 5 s); first error: HTTP 503 ServiceUnavailable | info |
| Dm | median / worst over the runs |  | client phase 22.79 s / 22.86 s; slowest PUT 22.79 s / 22.86 s; SDK phase 22.25 s / 22.31 s; max ACK gap 11.32 s / 12.12 s; failed attempts 0; failed uploads 36 | info |
| Ds | zero-progress windows >= 5 s | none | 5 (longest window 8.31 s) | FAIL |
| Df | failed uploads | 0 | 36 | FAIL |

0 passed, 2 failed, 4 informational

BURST_SUMMARY label=rustfs-nogate phase_median_s=22.793 phase_worst_s=22.856 slowest_put_median_s=22.793 slowest_put_worst_s=22.856 ack_gap_median_s=11.324 ack_gap_worst_s=12.120 stalls=5 failures=36
