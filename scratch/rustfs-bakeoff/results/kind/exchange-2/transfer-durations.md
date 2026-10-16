| Kind | Transfers | p50 (s) | p95 (s) | max (s) | over 5 s |
|---|---|---|---|---|---|
| pull (sandbox) | 94 | 0.05 | 0.38 | 0.47 | 0 |
| pull (template) | 27 | 0.15 | 0.85 | 0.88 | 0 |
| push (sandbox) | 149 | 0.07 | 1.20 | 10.55 | 4 |
| push_btrfs_send (sandbox) | 45 | 0.07 | 0.09 | 3.76 | 0 |
| all | 315 | 0.07 | 1.17 | 10.55 | 4 |

Transfers over 5 s:

- 2026-10-16T09:31:12.620688946Z push (sandbox): 10.37 s, 16448.0 MiB
- 2026-10-16T10:01:47.009338069Z push (sandbox): 10.51 s, 16448.0 MiB
- 2026-10-16T10:02:04.339615077Z push (sandbox): 10.52 s, 16448.0 MiB
- 2026-10-16T09:30:57.801721718Z push (sandbox): 10.55 s, 16448.0 MiB

Failed jobs logged: 8
- 2026-10-16T09:39:29.055885563Z push_btrfs_send job 01a14414-a12f-7668-93c2-db1dac728943 (None)
- 2026-10-16T09:39:29.646223832Z push job 01a14414-a105-71fb-86a3-8a36496ce3f8 (None)
- 2026-10-16T09:39:30.363973629Z push job 01a14414-a0fc-73cd-8418-04162d50a8ba (None)
- 2026-10-16T09:39:36.293152641Z push job 01a14414-a105-71fb-86a3-8a36496ce3f8 (None)
- 2026-10-16T09:39:37.335407221Z push_btrfs_send job 01a14414-a12f-7668-93c2-db1dac728943 (None)
- 2026-10-16T09:39:37.822752552Z push job 01a14414-a0fc-73cd-8418-04162d50a8ba (None)
- 2026-10-16T10:07:12.055545324Z push job 01a1442e-0fca-7660-96fb-d6caa9826d54 (None)
- 2026-10-16T10:07:13.968912074Z push job 01a1442e-0fa3-75c7-ac0d-ec4b3c761be4 (None)
