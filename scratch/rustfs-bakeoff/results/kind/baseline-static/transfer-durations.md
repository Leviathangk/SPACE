| Kind | Transfers | p50 (s) | p95 (s) | max (s) | over 5 s |
|---|---|---|---|---|---|
| pull (sandbox) | 94 | 0.03 | 0.37 | 0.38 | 0 |
| pull (template) | 24 | 0.05 | 0.58 | 0.59 | 0 |
| push (sandbox) | 149 | 0.03 | 1.18 | 10.70 | 4 |
| push_btrfs_send (sandbox) | 45 | 0.05 | 0.06 | 2.94 | 0 |
| all | 312 | 0.03 | 1.14 | 10.70 | 4 |

Transfers over 5 s:

- 2026-10-16T10:50:57.851457971Z push (sandbox): 10.40 s, 16448.0 MiB
- 2026-10-16T10:50:42.682398370Z push (sandbox): 10.43 s, 16448.0 MiB
- 2026-10-16T11:22:11.173453876Z push (sandbox): 10.50 s, 16448.0 MiB
- 2026-10-16T11:21:55.622348102Z push (sandbox): 10.70 s, 16448.0 MiB

Failed jobs logged: 7
- 2026-10-16T10:59:35.782486392Z push job 01a1445e-02e2-76ad-a9df-7a5c4e14bcbb (None)
- 2026-10-16T10:59:36.284086635Z push_btrfs_send job 01a1445e-032f-7139-b1e2-4f13c3a84219 (None)
- 2026-10-16T10:59:37.585744145Z push job 01a1445e-02f5-76cc-892e-d34eb1d0117a (None)
- 2026-10-16T10:59:42.864485773Z push_btrfs_send job 01a1445e-032f-7139-b1e2-4f13c3a84219 (None)
- 2026-10-16T10:59:45.094141189Z push job 01a1445e-02e2-76ad-a9df-7a5c4e14bcbb (None)
- 2026-10-16T10:59:46.305931750Z push job 01a1445e-02f5-76cc-892e-d34eb1d0117a (None)
- 2026-10-16T11:27:03.580179048Z push job 01a14477-268c-7743-b5b1-ea8da52f867b (None)
