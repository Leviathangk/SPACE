
### Early refusals of a large single PUT — versitygw v1.9.0 standalone IAM

| # | Check | Expected | Observed | Result |
|---|---|---|---|---|
| R1 | 20 MiB put_file refused early: static admin keys, a bucket that does not exist | info | after the client's retries: HTTP status 7/20, connection reset 13/20 | info |
| R2 | 20 MiB put_file refused early: static admin key, wrong secret | info | after the client's retries: HTTP status 0/20, connection reset 20/20 | info |
| R3 | 20 MiB put_file refused early: read-only session (policy refusal) | info | after the client's retries: HTTP status 2/20, connection reset 18/20 | info |

0 passed, 0 failed, 3 informational

