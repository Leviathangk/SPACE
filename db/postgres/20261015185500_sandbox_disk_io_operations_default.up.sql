SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

-- Match the platform default of 10,000 disk operations per second
-- (DEFAULT_DISK_IO_MAX_OPERATIONS_PER_SEC). Application writers bind both
-- limits, so this default only serves inserts that omit them, which must not
-- disagree with the platform. SET DEFAULT changes the catalog only: it
-- rewrites no rows, and existing sandboxes keep their stored limits. The pool
-- tables follow in a separate migration so a lock wait on this hot table does
-- not also hold them.
ALTER TABLE public.sandboxes
    ALTER COLUMN disk_io_max_operations_per_sec SET DEFAULT 10000;
