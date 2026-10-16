- ecw: SUITE: 77 passed, 0 warned, 0 failed, 1 inconclusive or infrastructure errors
- native: SUITE: 24 passed, 0 warned, 0 failed, 0 inconclusive or infrastructure errors

| Surface | Verdict | Scenarios |
|---|---|---|
| ecw | infra_error | 1 |
| ecw | pass | 77 |
| native | pass | 24 |

Not passed: 1
- ecw `local.ecw_template_build_from_oci_boots`: infra_error: BuildException: storage admission requires 55834574848 available bytes but found 35197800448
- `local.pending_backup_census_reports_blocked_upload`: pass (ecw)
- `native_recovery.slow_operation_does_not_block_later_work`: pass (native)
