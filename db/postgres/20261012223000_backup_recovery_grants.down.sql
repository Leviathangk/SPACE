DROP INDEX snapshots_recovery_grant;
ALTER TABLE snapshots DROP COLUMN recovery_grant_id;
DROP TABLE backup_recovery_grants;
