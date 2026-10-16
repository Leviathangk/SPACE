
### Early refusals of a large single PUT — versitygw v1.8.0, Kind static mode (no IAM service)

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| R1 | 20 MiB put_file refused early: static admin keys, a bucket that does not exist | info | after the client's retries: HTTP status 8/20, connection reset 10/20 | info |
| R2 | 20 MiB put_file refused early: static admin key, wrong secret | info | after the client's retries: HTTP status 10/20, connection reset 10/20 | info |

0 passed, 0 failed, 2 informational

