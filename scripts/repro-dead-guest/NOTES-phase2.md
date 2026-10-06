# Phase 2 working notes (natural guest death; restart attestation effect)

Running log, kept so the work survives an idle release. Final write-up:
`REPORT-guest-death-natural.md`.

- main 84a1d9ea; node-stack image sha256:95fa398c…, space-sbxmgr-svc 690945c7…,
  space-nodemgr-svc fbeffec2… on both sandbox workers (rolled 14:19Z).
- The Kind containers restarted at ~14:12Z (outside this task); every VM died, and the
  three earlier `repro-rst-*` probes went `failed`. Their pre-restart logs
  (11:20–13:32Z) show all three Degraded from the 11:19 restart, then 10 auto-suspend
  attempts each failing with `failed to connect to spaced bridge guest at firecracker
  vsock …/v.sock:5002: transport error`, then the budget park. None recovered.

## Experiment 2 trigger matrix (worker = restarted node, worker2 = control unless noted)

| id | restart | stranded? |
|---|---|---|
| T4 | kill spaced-host for 2 sandboxes (watchdog respawns it in ~5 s) | no |
| T2 | sbxmgr only, in place (`restart-one.sh sbxmgr`) | no (HP re-init ~80 s) |
| T3 | nodemgr only, in place (`restart-one.sh nodemgr`) | no (one transient 503, one 15 s start) |
| T1b | container restart in the same pod (kill credmgr; entrypoint exits) | 4/4 (2 fresh, 2 restored) |
| T1 | pod replacement (`kubectl delete pod`) on worker2 | 5/5 |
| P-T1b / P-T1 | same, with worker /space/cache = hostPath + Bidirectional | 0/6 |

Mechanism (mountcheck.sh): after T1b/T1 the entrypoint attaches a new loop device
to the same `/var/lib/space-cache.img` and mounts a second btrfs instance (same
fsid, new st_dev). The pod resolves `…/jails/firecracker/<id>/root/v.sock` as
`<new dev>:<ino>`; the VM listens on `<old dev>:<ino>`.

Clearing attempts on stranded guests: in-place sbxmgr restart (re-attestation)
no; spaced-host respawn no; explicit suspend -> Firecracker API `connection
refused` -> `fc_pause_failed` -> quarantined -> API `failed`; stop+start yes
(cold boot on the current mount). Sandboxes created/restored after the restart
are fine until their node's next restart.

## Between experiments (15:29–15:33Z)

- Stranded controls: e2a-1 stranded 14:29:13 -> 15:29:36Z (60 min) with no recovery;
  e2a-3 47 min. Raw CONNECT 5002 OK on every sample. All exp-2 sandboxes then deleted.
- dmesg on VM exit: the second instance's "last unmount" names a different fsid
  (e60a4e99… vs 3eb62a85…), consistent with btrfs temp-fsid for a duplicate mount.
- Workers returned to main's chart (`reset-cache-mounts.sh`; DaemonSet template
  identical to the pre-patch rendering). `btrfs scrub` then found 3944 / 2964
  uncorrectable csum errors on the two worker caches (they read 15 / 26 at the 14:12Z
  mount). Both cache images were recreated (`recreate-cache-images.sh`), scrub-clean,
  and the seed template was re-pulled before experiment 1.

## Experiment 1 (started 15:33:31Z)

`run-exp1.sh ~/repro-logs/phase2/e1/run 12` + `monitor.sh ~/repro-logs/phase2/e1/mon 60`.
12 sandboxes, 2 vCPU / 2048 MiB / 4096 MiB disk, memhog 200 -> 1500 MB over 4 h,
warm 10 min, then 3 cycles (odd: cross,same,cross; even: same,cross,same) of
suspend -> portable -> forced restore -> 60 min busy. Expected end ~18:50Z, then a
stress variant.
