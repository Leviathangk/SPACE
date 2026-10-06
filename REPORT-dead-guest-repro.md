# Dead-guest auto-suspend reproduction on Kind — DRAFT (OLD run in progress)

Status: NEW run complete; OLD run started 2026-10-06T09:49Z. Final results,
tables and recommendation are filled in when the OLD window closes.

## NEW (main 45c545cc) — summary so far

Scenario: native sandbox, `access_idle` → `suspend` at 90 s; inside the guest a
detached root loop SIGSTOPs `spaced.service`'s MainPID for 1200–4800 s
(uniform) and SIGCONTs it for 2 s. Six stalled sandboxes plus one control.

- Control: suspended on the first attempt, 0–1 s after it.
- 5/6 stalled sandboxes: 10 × `suspend_failed`
  (`guest_filesystem_flush`/`guest_fs_flush_failed`), then
  `auto-suspend retry budget exhausted; slow-retrying ~every 6h` at 09:09–09:12Z
  (~68–71 min after the first attempt); still `running` and `Degraded` at the
  09:45Z window end; `space.sandbox.guest.health{lifecycle:started,health:degraded}`
  = 5 across the two workers.
- 1/6 suspended on attempt 7 (a CONT landed inside a 180 s flush window).
