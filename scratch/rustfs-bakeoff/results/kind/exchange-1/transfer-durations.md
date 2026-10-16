| Kind | Transfers | p50 (s) | p95 (s) | max (s) | over 5 s |
|---|---|---|---|---|---|
| pull (sandbox) | 94 | 0.05 | 0.36 | 0.46 | 0 |
| pull (template) | 27 | 0.08 | 0.90 | 1.10 | 0 |
| push (sandbox) | 146 | 0.06 | 1.21 | 10.65 | 4 |
| push_btrfs_send (sandbox) | 44 | 0.07 | 0.08 | 0.18 | 0 |
| all | 311 | 0.06 | 1.16 | 10.65 | 4 |

Transfers over 5 s:

- 2026-10-16T08:23:01.407478252Z push (sandbox): 10.31 s, 16448.0 MiB
- 2026-10-16T08:54:35.420703220Z push (sandbox): 10.57 s, 16448.0 MiB
- 2026-10-16T08:23:16.564054473Z push (sandbox): 10.57 s, 16448.0 MiB
- 2026-10-16T08:54:52.797308339Z push (sandbox): 10.65 s, 16448.0 MiB

Failed jobs logged: 6
- 2026-10-16T08:31:49.544805090Z push job 01a143d6-bd29-71a3-8d4d-6e73a11dd78f (None)
- 2026-10-16T08:31:51.262217608Z push job 01a143d6-bd1b-7106-b4dd-697f729cdba9 (None)
- 2026-10-16T08:31:52.615611218Z push_btrfs_send job 01a143d6-bd5b-74df-9957-67337a2a1e8d (None)
- 2026-10-16T08:31:55.184189357Z push job 01a143d6-bd29-71a3-8d4d-6e73a11dd78f (None)
- 2026-10-16T08:32:00.935425389Z push job 01a143d6-bd1b-7106-b4dd-697f729cdba9 (None)
- 2026-10-16T08:32:01.546494614Z push_btrfs_send job 01a143d6-bd5b-74df-9957-67337a2a1e8d (None)
