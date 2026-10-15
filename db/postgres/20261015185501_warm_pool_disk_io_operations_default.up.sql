SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

-- Same platform default as the sandboxes column in the previous migration.
-- Catalog-only: existing pool configurations, slots and policies keep their
-- stored limits.
ALTER TABLE public.kubernetes_pool_configs
    ALTER COLUMN disk_io_max_operations_per_sec SET DEFAULT 10000;

ALTER TABLE public.kubernetes_pool_slots
    ALTER COLUMN disk_io_max_operations_per_sec SET DEFAULT 10000;

COMMENT ON COLUMN public.warm_pool_policies.disk_io_max_operations_per_sec IS 'Optional override; NULL inherits the platform default of 10000 operations per second.';
