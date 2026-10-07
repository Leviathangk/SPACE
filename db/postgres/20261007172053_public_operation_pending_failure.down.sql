SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

-- Reverting drops failures still waiting on compensation. Roll the
-- cluster-controller back with this migration: an older dispatcher re-drives
-- such an operation as a fresh attempt, which re-derives its failure.
ALTER TABLE public.public_operations
    DROP COLUMN IF EXISTS pending_failure_at,
    DROP COLUMN IF EXISTS pending_failure_json;
