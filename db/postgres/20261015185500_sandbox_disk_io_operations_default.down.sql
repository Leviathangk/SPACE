SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

ALTER TABLE public.sandboxes
    ALTER COLUMN disk_io_max_operations_per_sec SET DEFAULT 1000;
