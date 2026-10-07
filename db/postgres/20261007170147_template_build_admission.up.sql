SET LOCAL lock_timeout = '5s';
SET LOCAL statement_timeout = '60s';

-- Tenant template-build admission and retention state.
--
-- Organization API keys can create build specifications, upload build inputs,
-- and dispatch builds. Each of those holds shared capacity: control-plane
-- rows, object storage, and builder slots. These tables attribute that
-- capacity to the authenticated organization so the API can bound it per
-- organization and cluster-wide before accepting more, and so abandoned state
-- expires. Every admission decision runs under one transaction-scoped
-- advisory lock, so concurrent API replicas cannot overshoot a limit.
--
-- They live beside, not inside, `template_build_jobs` and
-- `template_build_blobs`: those keep their shapes, and every index here is on
-- a table this migration creates.

-- One row per tenant build specification (`POST /v3/templates`). `pending`
-- until its build is dispatched, `active` while the dispatched build may hold
-- builder capacity, `finished` once the API observed a terminal state.
-- `expires_at` is when the current state stops counting against limits: the
-- specification TTL, the longest an unobserved build can run, or the record
-- retention. Expired rows and their `template_build_jobs` row are swept.
CREATE TABLE IF NOT EXISTS template_build_admissions (
    template_id TEXT NOT NULL,
    build_id    TEXT NOT NULL,
    org_id      UUID NOT NULL,
    state       TEXT NOT NULL CHECK (state IN ('pending', 'active', 'finished')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at  TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (template_id, build_id),
    FOREIGN KEY (template_id, build_id)
        REFERENCES template_build_jobs (template_id, build_id) ON DELETE CASCADE
);

-- Per-organization and cluster-wide counts by state, and the expiry sweep.
CREATE INDEX IF NOT EXISTS idx_template_build_admissions_org_state
    ON template_build_admissions (org_id, state, expires_at);
CREATE INDEX IF NOT EXISTS idx_template_build_admissions_state_expires_at
    ON template_build_admissions (state, expires_at);

-- Build-input bytes charged to the organization that uploaded them. Every
-- upload that may store an object holds one row, inserted as `reserved` with
-- the most the upload may store before the API forwards any byte, and settled
-- to `stored` with the stored size once the builder reports it. The charge is
-- per upload, not per mapping: a reupload that replaces a mapping, or an
-- upload whose registration is refused after the builder stored it, still
-- stored an object. Only an upload that never committed to the builder
-- deletes its row. `expires_at` ends the charge a fixed window after the
-- upload, also when the API died mid-upload. That window bounds what an
-- organization can upload per window; it is not a record of what the bucket
-- still stores, which this table neither observes nor deletes. `created_at`
-- bounds how long a `reserved` row counts as an upload in flight.
CREATE TABLE IF NOT EXISTS template_build_input_charges (
    -- Application-generated UUIDv7 (Uuid::now_v7()).
    id         UUID PRIMARY KEY,
    org_id     UUID NOT NULL,
    state      TEXT NOT NULL CHECK (state IN ('reserved', 'stored')),
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL
);

-- Per-organization byte sums and in-flight counts, and the expiry sweep.
CREATE INDEX IF NOT EXISTS idx_template_build_input_charges_org_state
    ON template_build_input_charges (org_id, state, created_at);
CREATE INDEX IF NOT EXISTS idx_template_build_input_charges_expires_at
    ON template_build_input_charges (expires_at);

-- Lifetime of one upload mapping. A mapping is honoured only while this row
-- is unexpired; the sweep deletes both. The build manager keeps the shared
-- input object written recently enough to outlive it (see
-- `SHARED_INPUT_LEASE_RENEWAL_AGE`), so an expired mapping is the end of the
-- control plane's reference to that object. Bytes are charged separately, in
-- `template_build_input_charges`.
CREATE TABLE IF NOT EXISTS template_build_input_retention (
    template_id TEXT NOT NULL,
    files_hash  TEXT NOT NULL,
    org_id      UUID NOT NULL,
    expires_at  TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (template_id, files_hash),
    FOREIGN KEY (template_id, files_hash)
        REFERENCES template_build_blobs (template_id, files_hash) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_template_build_input_retention_org_id
    ON template_build_input_retention (org_id);
CREATE INDEX IF NOT EXISTS idx_template_build_input_retention_expires_at
    ON template_build_input_retention (expires_at);
