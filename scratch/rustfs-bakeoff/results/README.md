# Raw results

Tables as the probe and `perf.sh` printed them, from one Linux devbox, both
stores run with the Kind chart's limits (1 CPU, 4 GiB) unless a file says
otherwise. REPORT.md, one directory up, summarises and interprets them.

- `a-*`, `b1-*`, `c-*`: criteria A, B1 and C (`c-expiry-rustfs.md`: session expiry).
- `b2-*`: criterion B2, with the store's own log of every attempt per freeze.
  These two runs had no CPU or memory limit.
- `d-perf-*`: criterion D, each burst run on a fresh container.
- `d-variant-*`: extra RustFS (and one versitygw) bursts with other settings,
  explained in REPORT.md. Variants whose third run reports a mass of failed
  uploads in its SDK phase ran three bursts on one container, and the Docker
  disk filled because RustFS frees deleted objects only every five minutes;
  those runs are not counted.

The variant files were recorded before the burst's stall measure was narrowed
to windows in which some upload body was still being sent: their "longest
zero-progress window" also counts time spent waiting for acknowledgements.
`d-perf-*` use the narrowed measure.
