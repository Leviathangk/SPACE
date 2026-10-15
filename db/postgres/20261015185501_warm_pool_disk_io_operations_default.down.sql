SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

ALTER TABLE public.kubernetes_pool_configs
    ALTER COLUMN disk_io_max_operations_per_sec SET DEFAULT 1000;

ALTER TABLE public.kubernetes_pool_slots
    ALTER COLUMN disk_io_max_operations_per_sec SET DEFAULT 1000;

COMMENT ON COLUMN public.warm_pool_policies.disk_io_max_operations_per_sec IS 'Optional override; NULL inherits the platform default of 1000 operations per second.';
