
### D. Upload burst — rustfs-4cpu-nogate-run1

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| D1 | run 1: 36 x 128 MiB at once |  | client phase 15.00 s, slowest PUT 15.00 s, 0 failed attempts, 0 failed uploads; SDK phase 14.43 s, max ACK gap 12.00 s, 0 failed uploads; longest zero-progress window 6.31 s (1 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 15.00 s / 15.00 s; slowest PUT 15.00 s / 15.00 s; SDK phase 14.43 s / 14.43 s; max ACK gap 12.00 s / 12.00 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress windows >= 5 s | none | 1 (longest window 6.31 s) | FAIL |
| Df | failed uploads | 0 | 0 | PASS |

1 passed, 1 failed, 2 informational

BURST_SUMMARY label=rustfs-4cpu-nogate-run1 phase_median_s=14.996 phase_worst_s=14.996 slowest_put_median_s=14.996 slowest_put_worst_s=14.996 ack_gap_median_s=11.997 ack_gap_worst_s=11.997 stalls=1 failures=0

### D. Upload burst — rustfs-4cpu-nogate-run2

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| D1 | run 1: 36 x 128 MiB at once |  | client phase 15.37 s, slowest PUT 15.37 s, 0 failed attempts, 0 failed uploads; SDK phase 13.48 s, max ACK gap 9.21 s, 0 failed uploads; longest zero-progress window 4.10 s (0 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 15.37 s / 15.37 s; slowest PUT 15.37 s / 15.37 s; SDK phase 13.48 s / 13.48 s; max ACK gap 9.21 s / 9.21 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress windows >= 5 s | none | 0 (longest window 4.10 s) | PASS |
| Df | failed uploads | 0 | 0 | PASS |

2 passed, 0 failed, 2 informational

BURST_SUMMARY label=rustfs-4cpu-nogate-run2 phase_median_s=15.372 phase_worst_s=15.372 slowest_put_median_s=15.372 slowest_put_worst_s=15.372 ack_gap_median_s=9.211 ack_gap_worst_s=9.211 stalls=0 failures=0

### D. Upload burst — rustfs-4cpu-nogate-run3

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| D1 | run 1: 36 x 128 MiB at once |  | client phase 14.52 s, slowest PUT 14.52 s, 0 failed attempts, 0 failed uploads; SDK phase 14.28 s, max ACK gap 11.32 s, 0 failed uploads; longest zero-progress window 5.21 s (1 >= 5 s) | info |
| Dm | median / worst over the runs |  | client phase 14.52 s / 14.52 s; slowest PUT 14.52 s / 14.52 s; SDK phase 14.28 s / 14.28 s; max ACK gap 11.32 s / 11.32 s; failed attempts 0; failed uploads 0 | info |
| Ds | zero-progress windows >= 5 s | none | 1 (longest window 5.21 s) | FAIL |
| Df | failed uploads | 0 | 0 | PASS |

1 passed, 1 failed, 2 informational

BURST_SUMMARY label=rustfs-4cpu-nogate-run3 phase_median_s=14.518 phase_worst_s=14.518 slowest_put_median_s=14.518 slowest_put_worst_s=14.518 ack_gap_median_s=11.315 ack_gap_worst_s=11.315 stalls=1 failures=0
