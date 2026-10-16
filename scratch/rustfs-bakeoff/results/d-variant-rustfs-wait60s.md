
### D. Upload burst — rustfs-wait60s

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| D1 | run 1: 36 x 128 MiB at once |  | client phase 17.29 s, slowest PUT 17.29 s, 8 failed attempts, 0 failed uploads; SDK phase 20.79 s, max ACK gap 8.49 s, 3 failed uploads; longest zero-progress window 5.41 s (1 >= 5 s); first error: HTTP 0 dispatch-failure | info |
| D2 | run 2: 36 x 128 MiB at once |  | client phase 20.89 s, slowest PUT 20.89 s, 10 failed attempts, 3 failed uploads; SDK phase 20.39 s, max ACK gap 11.26 s, 3 failed uploads; longest zero-progress window 5.91 s (1 >= 5 s); first error: S3 put_object failed: dispatch failure: timeout: minimum throughput was specified at 1 B/s, but throughput of 0 B/s was observed (DispatchFailure(DispatchFailur | info |
| D3 | run 3: 36 x 128 MiB at once |  | client phase 20.97 s, slowest PUT 20.97 s, 11 failed attempts, 3 failed uploads; SDK phase 12.75 s, max ACK gap 3.22 s, 35 failed uploads; longest zero-progress window 5.31 s (1 >= 5 s); first error: S3 put_object failed: dispatch failure: timeout: minimum throughput was specified at 1 B/s, but throughput of 0 B/s was observed (DispatchFailure(DispatchFailur | info |
| Dm | median / worst over the runs |  | client phase 20.89 s / 20.97 s; slowest PUT 20.89 s / 20.97 s; SDK phase 20.39 s / 20.79 s; max ACK gap 8.49 s / 11.26 s; failed attempts 29; failed uploads 47 | info |
| Ds | zero-progress windows >= 5 s | none | 3 (longest window 5.91 s) | FAIL |
| Df | failed uploads | 0 | 47 | FAIL |

0 passed, 2 failed, 4 informational

BURST_SUMMARY label=rustfs-wait60s phase_median_s=20.890 phase_worst_s=20.969 slowest_put_median_s=20.890 slowest_put_worst_s=20.969 ack_gap_median_s=8.494 ack_gap_worst_s=11.257 stalls=3 failures=47
