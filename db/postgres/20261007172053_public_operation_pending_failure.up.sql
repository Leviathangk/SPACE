SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

-- A failure the dispatcher has decided on but not yet recorded, because the
-- executor is still undoing what the failed operation left behind (for a
-- failed sandbox start, the runtime its last attempt materialized). The
-- operation stays queued while this is set, and each claim retries only the
-- compensation; once it completes, this exact failure is recorded. NULL for
-- every operation that is not in that phase.
ALTER TABLE public.public_operations
    ADD COLUMN IF NOT EXISTS pending_failure_json jsonb,
    -- When the failure was first deferred, so compensation that never
    -- completes is bounded and the failure is still reported.
    ADD COLUMN IF NOT EXISTS pending_failure_at timestamp with time zone;
