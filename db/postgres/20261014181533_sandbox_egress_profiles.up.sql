SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

ALTER TABLE sandboxes
    ADD COLUMN egress_profiles_json JSONB NOT NULL DEFAULT '{}'::jsonb;
