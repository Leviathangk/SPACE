
### C. Exchange requirements — versitygw

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| C1 | AssumeRole (Policy, DurationSeconds 3600) signed by the vend identity | credentials, ~3600 s | HTTP 405 - | FAIL |

0 passed, 1 failed, 0 informational

