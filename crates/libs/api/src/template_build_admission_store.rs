//! Tenant template-build admission and retention.
//!
//! An organization's API key can record build specifications, upload build
//! inputs, and dispatch builds. Each holds shared capacity: control-plane rows,
//! object storage, and builder slots. This store attributes that capacity to
//! the authenticated organization and admits more only below a per-organization
//! and a cluster-wide limit ([`TemplateBuildLimits`]). Every admission runs in
//! one transaction under one advisory lock, so concurrent API replicas cannot
//! both take the last unit of a limit. The same transaction first sweeps state
//! whose lifetime has ended, which is what returns capacity:
//!
//! * a specification whose build was never dispatched expires after
//!   [`TemplateBuildLimits::spec_ttl`];
//! * a dispatched build stops counting once the API observes its terminal
//!   state, or after [`TemplateBuildLimits::build_lease`] if no client polls
//!   it; its record is kept for [`TemplateBuildLimits::build_record_retention`]
//!   so status polls keep working;
//! * an upload mapping is honoured for [`TemplateBuildLimits::input_mapping_ttl`]
//!   after it is recorded;
//! * an upload's bytes are charged to its organization from before the first
//!   byte is forwarded until [`TemplateBuildLimits::input_charge_window`]
//!   after it settles. The charge is per upload, not per mapping: replacing a
//!   mapping or being refused at registration does not delete an object the
//!   builder already stored. Only an upload that never committed to the
//!   builder gives its charge back. This bounds the bytes an organization can
//!   upload per window; it does not observe or delete stored objects, which
//!   remain the bucket's to expire (see `docs/configuration.md`).
//!
//! Operator builds (the admin routes) do not pass through here; build-manager
//! keeps them a reserved slot of their own.

use std::time::Duration;

use pplx_space_common_core::{
    sql,
    types::{OrgId, TemplateName},
};
use pplx_space_metrics::template::{
    TemplateBuildAdmissionOutcome, TemplateBuildAdmissionResource, TemplateBuildGcKind,
};
use sqlx::{PgConnection, PgPool};

use crate::template_build_jobs_store::{
    BuildSpec, insert_build_spec, template_build_owner_matches,
};

/// Advisory-lock key serializing every tenant template-build admission.
const ADMISSION_LOCK: &str = "space.template_build.admission";

/// Bounds on tenant template-build state. [`Default`] is the production
/// policy; the counts are deliberately well above what one organization's
/// ordinary build traffic needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateBuildLimits {
    /// How long a specification may wait for its build to be dispatched.
    pub spec_ttl: Duration,
    pub org_pending_specs: u32,
    pub global_pending_specs: u32,
    /// Longest a dispatched build counts as active without a terminal status
    /// observation: build-manager's tenant queue timeout plus its execution
    /// timeout, with margin.
    pub build_lease: Duration,
    pub org_active_builds: u32,
    pub global_active_builds: u32,
    /// How long a finished build's record stays readable to status polls.
    pub build_record_retention: Duration,
    /// How long a reserved upload counts as in flight. Outlives the upload
    /// deadline; an upload past it still holds its bytes, only not its slot.
    pub upload_in_flight_window: Duration,
    /// How long an upload's bytes stay charged after it settles. Chosen to
    /// exceed a 14-day current-version plus 1-day noncurrent-version bucket
    /// expiry for build inputs, but it is a charging window, not a bound on
    /// physical retention: lifecycle expiry runs asynchronously with no
    /// completion guarantee, and a bucket without such a rule keeps objects
    /// indefinitely. Stored objects are neither observed nor deleted here.
    pub input_charge_window: Duration,
    pub org_uploads_in_flight: u32,
    pub global_uploads_in_flight: u32,
    /// How long an upload mapping is honoured. Build-manager keeps the shared
    /// input object written recently enough to outlive it.
    pub input_mapping_ttl: Duration,
    pub org_input_mappings: u32,
    pub global_input_mappings: u32,
    pub org_input_bytes: u64,
    pub global_input_bytes: u64,
    /// The most one upload may store.
    pub max_upload_bytes: u64,
}

const GIB: u64 = 1024 * 1024 * 1024;
const HOUR: Duration = Duration::from_secs(60 * 60);
const DAY: Duration = Duration::from_secs(24 * 60 * 60);

impl Default for TemplateBuildLimits {
    fn default() -> Self {
        Self {
            spec_ttl: DAY,
            org_pending_specs: 32,
            global_pending_specs: 2048,
            build_lease: 13 * HOUR,
            org_active_builds: 4,
            global_active_builds: 32,
            build_record_retention: 30 * DAY,
            upload_in_flight_window: HOUR,
            input_charge_window: 16 * DAY,
            org_uploads_in_flight: 2,
            global_uploads_in_flight: 16,
            input_mapping_ttl: 7 * DAY,
            org_input_mappings: 1024,
            global_input_mappings: 65_536,
            org_input_bytes: 64 * GIB,
            global_input_bytes: 2048 * GIB,
            max_upload_bytes: pplx_space_build_mgr_client::BLOB_UPLOAD_LIMIT_BYTES,
        }
    }
}

/// Which limit refused an admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionScope {
    Organization,
    Global,
}

impl AdmissionScope {
    const fn outcome(self) -> TemplateBuildAdmissionOutcome {
        match self {
            Self::Organization => TemplateBuildAdmissionOutcome::OrgLimit,
            Self::Global => TemplateBuildAdmissionOutcome::GlobalLimit,
        }
    }
}

/// What an organization ran out of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionLimit {
    PendingSpecs,
    ActiveBuilds,
    UploadsInFlight,
    InputMappings,
    InputBytes,
}

impl AdmissionLimit {
    const fn resource(self) -> TemplateBuildAdmissionResource {
        match self {
            Self::PendingSpecs => TemplateBuildAdmissionResource::BuildSpec,
            Self::ActiveBuilds => TemplateBuildAdmissionResource::Build,
            Self::UploadsInFlight | Self::InputMappings | Self::InputBytes => {
                TemplateBuildAdmissionResource::Upload
            }
        }
    }

    const fn description(self) -> &'static str {
        match self {
            Self::PendingSpecs => "pending template build",
            Self::ActiveBuilds => "active template build",
            Self::UploadsInFlight => "concurrent template file upload",
            Self::InputMappings => "retained template file",
            Self::InputBytes => "retained template file byte",
        }
    }

    /// When retrying can first succeed: a running upload or build frees its
    /// unit within minutes, while retained inputs and pending specifications
    /// free theirs only as they expire.
    pub(crate) const fn retry_after(self) -> Duration {
        match self {
            Self::UploadsInFlight => Duration::from_secs(5),
            Self::ActiveBuilds => Duration::from_secs(30),
            Self::PendingSpecs | Self::InputMappings | Self::InputBytes => Duration::from_secs(300),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{} {} limit reached; retry later", match .scope {
    AdmissionScope::Organization => "organization",
    AdmissionScope::Global => "cluster",
}, .limit.description())]
pub(crate) struct AdmissionRejection {
    pub limit: AdmissionLimit,
    pub scope: AdmissionScope,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum TemplateBuildAdmissionError {
    #[error("template build admission storage failed: {0}")]
    Storage(#[from] sqlx::Error),
    #[error(transparent)]
    Rejected(AdmissionRejection),
    #[error("template build inputs belong to another organization")]
    OwnershipConflict,
    #[error("this template build was already started")]
    AlreadyStarted,
    #[error("no pending build for this (templateID, buildID); call POST /v3/templates first")]
    NoPendingBuild,
}

/// One upload's charge against its organization's input quota.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InputReservation {
    id: uuid::Uuid,
    /// The most this upload may store.
    pub max_bytes: u64,
}

/// Recording an upload against its reservation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RecordInputError {
    #[error("template build blob storage failed: {0}")]
    Storage(#[from] sqlx::Error),
    #[error("template build upload conflicts with existing ownership or content")]
    Conflict,
}

#[derive(Clone)]
pub(crate) struct TemplateBuildAdmissionStore {
    pool: PgPool,
    limits: TemplateBuildLimits,
}

impl TemplateBuildAdmissionStore {
    pub(crate) fn new(pool: PgPool, limits: TemplateBuildLimits) -> Self {
        Self { pool, limits }
    }

    pub(crate) fn limits(&self) -> &TemplateBuildLimits {
        &self.limits
    }

    /// Retire expired state in its own transaction, so it is committed even
    /// when the admission that follows is refused. Counts filter on expiry
    /// themselves and stay exact whether or not a sweep has run.
    async fn sweep(&self) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sweep_expired(&mut tx, &self.limits).await?;
        tx.commit().await
    }

    /// Record a build specification as pending for `org_id`, if the
    /// organization and the cluster are below their pending limits.
    pub(crate) async fn admit_build_spec(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        build_id: &str,
        spec: BuildSpec,
    ) -> Result<(), TemplateBuildAdmissionError> {
        let limits = &self.limits;
        self.sweep().await?;
        let mut tx = self.pool.begin().await?;
        lock_admission(&mut tx).await?;
        if !template_build_owner_matches(&mut *tx, template_name, org_id).await? {
            return Err(TemplateBuildAdmissionError::OwnershipConflict);
        }
        let (org, global) = count_admissions(&mut tx, org_id, AdmissionState::Pending).await?;
        pplx_space_metrics::template::build_admitted_pending(
            TemplateBuildAdmissionResource::BuildSpec,
            global,
        );
        check_count(
            AdmissionLimit::PendingSpecs,
            org,
            limits.org_pending_specs,
            global,
            limits.global_pending_specs,
        )?;
        if !insert_build_spec(&mut *tx, org_id, template_name, build_id, spec).await? {
            return Err(TemplateBuildAdmissionError::OwnershipConflict);
        }
        sqlx::query(sql!(
            "INSERT INTO template_build_admissions
                 (template_id, build_id, org_id, state, expires_at)
             VALUES ($1, $2, $3, 'pending', NOW() + make_interval(secs => $4))
             ON CONFLICT (template_id, build_id) DO UPDATE
             SET expires_at = EXCLUDED.expires_at
             WHERE template_build_admissions.state = 'pending'
               AND template_build_admissions.org_id = EXCLUDED.org_id"
        ))
        .bind(template_name.as_str())
        .bind(build_id)
        .bind(org_id.as_uuid())
        .bind(limits.spec_ttl.as_secs_f64())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        admitted(TemplateBuildAdmissionResource::BuildSpec);
        Ok(())
    }

    /// Move `org_id`'s pending specification to an active build, if the
    /// organization and the cluster are below their active-build limits.
    /// Call before dispatching the build; [`Self::release_build`] undoes this
    /// when dispatch fails.
    pub(crate) async fn admit_build(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        build_id: &str,
    ) -> Result<(), TemplateBuildAdmissionError> {
        let limits = &self.limits;
        self.sweep().await?;
        let mut tx = self.pool.begin().await?;
        lock_admission(&mut tx).await?;
        let current = sqlx::query_as::<_, (String, bool)>(sql!(
            "SELECT state, expires_at > NOW()
             FROM template_build_admissions
             WHERE template_id = $1 AND build_id = $2 AND org_id = $3"
        ))
        .bind(template_name.as_str())
        .bind(build_id)
        .bind(org_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?;
        match current
            .as_ref()
            .map(|(state, fresh)| (state.as_str(), *fresh))
        {
            Some(("pending", true)) => {}
            Some(("pending", false)) => return Err(TemplateBuildAdmissionError::NoPendingBuild),
            Some(_) => {
                pplx_space_metrics::template::build_admission(
                    TemplateBuildAdmissionResource::Build,
                    TemplateBuildAdmissionOutcome::AlreadyActive,
                );
                return Err(TemplateBuildAdmissionError::AlreadyStarted);
            }
            None => {
                // A specification recorded before admission existed has no
                // row; it is admitted like any other while it is undispatched.
                let undispatched = sqlx::query_scalar::<_, bool>(sql!(
                    "SELECT daemon_job_id IS NULL
                     FROM template_build_jobs
                     WHERE template_id = $1
                       AND build_id = $2
                       AND (org_id = $3 OR org_id IS NULL)"
                ))
                .bind(template_name.as_str())
                .bind(build_id)
                .bind(org_id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?;
                match undispatched {
                    Some(true) => {}
                    Some(false) => return Err(TemplateBuildAdmissionError::AlreadyStarted),
                    None => return Err(TemplateBuildAdmissionError::NoPendingBuild),
                }
            }
        }
        let (org, global) = count_admissions(&mut tx, org_id, AdmissionState::Active).await?;
        pplx_space_metrics::template::build_admitted_pending(
            TemplateBuildAdmissionResource::Build,
            global,
        );
        check_count(
            AdmissionLimit::ActiveBuilds,
            org,
            limits.org_active_builds,
            global,
            limits.global_active_builds,
        )?;
        sqlx::query(sql!(
            "INSERT INTO template_build_admissions
                 (template_id, build_id, org_id, state, expires_at)
             VALUES ($1, $2, $3, 'active', NOW() + make_interval(secs => $4))
             ON CONFLICT (template_id, build_id) DO UPDATE
             SET state = 'active',
                 expires_at = EXCLUDED.expires_at"
        ))
        .bind(template_name.as_str())
        .bind(build_id)
        .bind(org_id.as_uuid())
        .bind(limits.build_lease.as_secs_f64())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        admitted(TemplateBuildAdmissionResource::Build);
        Ok(())
    }

    /// Return an admitted build whose dispatch failed to its pending
    /// specification, under the specification's original lifetime.
    pub(crate) async fn release_build(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        build_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(sql!(
            "UPDATE template_build_admissions
             SET state = 'pending',
                 expires_at = created_at + make_interval(secs => $4)
             WHERE template_id = $1
               AND build_id = $2
               AND org_id = $3
               AND state = 'active'"
        ))
        .bind(template_name.as_str())
        .bind(build_id)
        .bind(org_id.as_uuid())
        .bind(self.limits.spec_ttl.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record that `org_id`'s build reached a terminal state, freeing its
    /// active-build unit and starting its record retention. Returns whether
    /// this call made the transition.
    pub(crate) async fn finish_build(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        build_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(sql!(
            "UPDATE template_build_admissions
             SET state = 'finished',
                 expires_at = NOW() + make_interval(secs => $4)
             WHERE template_id = $1
               AND build_id = $2
               AND org_id = $3
               AND state = 'active'"
        ))
        .bind(template_name.as_str())
        .bind(build_id)
        .bind(org_id.as_uuid())
        .bind(self.limits.build_record_retention.as_secs_f64())
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Charge one upload by `org_id` before any byte is accepted. The charge
    /// holds the most the upload may store, which is the per-upload cap or
    /// the organization's or cluster's remaining input bytes, whichever is
    /// smallest, until [`Self::settle_input`] records the stored size.
    pub(crate) async fn reserve_input_upload(
        &self,
        org_id: OrgId,
    ) -> Result<InputReservation, TemplateBuildAdmissionError> {
        let limits = &self.limits;
        self.sweep().await?;
        let mut tx = self.pool.begin().await?;
        lock_admission(&mut tx).await?;
        let usage = sqlx::query_as::<_, (i64, i64, i64, i64, i64, i64)>(sql!(
            "SELECT
                 (SELECT COUNT(*) FROM template_build_input_charges
                  WHERE org_id = $1
                    AND state = 'reserved'
                    AND created_at > NOW() - make_interval(secs => $2)
                    AND expires_at > NOW()),
                 (SELECT COUNT(*) FROM template_build_input_charges
                  WHERE state = 'reserved'
                    AND created_at > NOW() - make_interval(secs => $2)
                    AND expires_at > NOW()),
                 (SELECT COUNT(*) FROM template_build_input_retention
                  WHERE org_id = $1 AND expires_at > NOW()),
                 (SELECT COUNT(*) FROM template_build_input_retention
                  WHERE expires_at > NOW()),
                 (SELECT COALESCE(SUM(size_bytes), 0)::BIGINT FROM template_build_input_charges
                  WHERE org_id = $1 AND expires_at > NOW()),
                 (SELECT COALESCE(SUM(size_bytes), 0)::BIGINT FROM template_build_input_charges
                  WHERE expires_at > NOW())"
        ))
        .bind(org_id.as_uuid())
        .bind(limits.upload_in_flight_window.as_secs_f64())
        .fetch_one(&mut *tx)
        .await?;
        let (org_uploads, global_uploads, org_mappings, global_mappings, org_bytes, global_bytes) =
            usage;
        let [
            org_uploads,
            global_uploads,
            org_mappings,
            global_mappings,
            org_bytes,
            global_bytes,
        ] = [
            org_uploads,
            global_uploads,
            org_mappings,
            global_mappings,
            org_bytes,
            global_bytes,
        ]
        .map(|value| u64::try_from(value).unwrap_or(0));
        pplx_space_metrics::template::build_input_retained_bytes(global_bytes);
        check_count(
            AdmissionLimit::UploadsInFlight,
            org_uploads,
            limits.org_uploads_in_flight,
            global_uploads,
            limits.global_uploads_in_flight,
        )?;
        // An upload in flight becomes a mapping; count it as one.
        check_count(
            AdmissionLimit::InputMappings,
            org_mappings + org_uploads,
            limits.org_input_mappings,
            global_mappings + global_uploads,
            limits.global_input_mappings,
        )?;
        let org_remaining = limits.org_input_bytes.saturating_sub(org_bytes);
        let global_remaining = limits.global_input_bytes.saturating_sub(global_bytes);
        if org_remaining == 0 || global_remaining == 0 {
            let scope = if org_remaining == 0 {
                AdmissionScope::Organization
            } else {
                AdmissionScope::Global
            };
            return Err(rejected(AdmissionLimit::InputBytes, scope));
        }
        let max_bytes = limits
            .max_upload_bytes
            .min(org_remaining)
            .min(global_remaining);
        let reservation = InputReservation {
            id: uuid::Uuid::now_v7(),
            max_bytes,
        };
        sqlx::query(sql!(
            "INSERT INTO template_build_input_charges (id, org_id, state, size_bytes, expires_at)
             VALUES ($1, $2, 'reserved', $3, NOW() + make_interval(secs => $4))"
        ))
        .bind(reservation.id)
        .bind(org_id.as_uuid())
        .bind(i64::try_from(max_bytes).unwrap_or(i64::MAX))
        .bind(limits.input_charge_window.as_secs_f64())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        admitted(TemplateBuildAdmissionResource::Upload);
        Ok(reservation)
    }

    /// Settle an upload's charge to the `size_bytes` the builder may have
    /// stored, for [`TemplateBuildLimits::input_charge_window`]. Call whenever
    /// the upload committed to the builder, whatever happens afterwards.
    pub(crate) async fn settle_input(
        &self,
        reservation: InputReservation,
        size_bytes: u64,
    ) -> Result<(), sqlx::Error> {
        let mut connection = self.pool.acquire().await?;
        settle_charge(&mut connection, reservation, size_bytes, &self.limits).await
    }

    /// Settle the upload's charge and record its mapping, retained for
    /// [`TemplateBuildLimits::input_mapping_ttl`], as one admission step: the
    /// upload's reserved mapping slot becomes its mapping without being free
    /// in between. The charge stays settled when the mapping is refused: the
    /// builder already stored the bytes.
    pub(crate) async fn record_input(
        &self,
        reservation: InputReservation,
        org_id: OrgId,
        template_name: &TemplateName,
        files_hash: &str,
        blob_sha256: &str,
        size_bytes: u64,
    ) -> Result<(), RecordInputError> {
        let mut tx = self.pool.begin().await?;
        lock_admission(&mut tx).await?;
        settle_charge(&mut tx, reservation, size_bytes, &self.limits).await?;
        if !template_build_owner_matches(&mut *tx, template_name, org_id).await? {
            tx.commit().await?;
            return Err(RecordInputError::Conflict);
        }
        // The SDK's file hash excludes archive metadata, so same-owner reuploads can
        // have different blob digests even when the file contents are unchanged.
        // Mappings recorded before uploads carried an owner have a NULL org_id;
        // the first owned upload claims them so later writers meet the owner check.
        let result = sqlx::query(sql!(
            "INSERT INTO template_build_blobs (template_id, files_hash, blob_sha256, org_id)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (template_id, files_hash) DO UPDATE
             SET blob_sha256 = EXCLUDED.blob_sha256,
                 org_id = EXCLUDED.org_id
             WHERE template_build_blobs.org_id IS NULL
                OR template_build_blobs.org_id = EXCLUDED.org_id"
        ))
        .bind(template_name.as_str())
        .bind(files_hash)
        .bind(blob_sha256)
        .bind(org_id.as_uuid())
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            tx.commit().await?;
            return Err(RecordInputError::Conflict);
        }
        sqlx::query(sql!(
            "INSERT INTO template_build_input_retention
                 (template_id, files_hash, org_id, expires_at)
             VALUES ($1, $2, $3, NOW() + make_interval(secs => $4))
             ON CONFLICT (template_id, files_hash) DO UPDATE
             SET org_id = EXCLUDED.org_id,
                 expires_at = EXCLUDED.expires_at"
        ))
        .bind(template_name.as_str())
        .bind(files_hash)
        .bind(org_id.as_uuid())
        .bind(self.limits.input_mapping_ttl.as_secs_f64())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Give back the charge of an upload that never committed to the builder,
    /// so it stored nothing. A settled charge is never given back.
    pub(crate) async fn release_input_reservation(
        &self,
        reservation: InputReservation,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(sql!(
            "DELETE FROM template_build_input_charges WHERE id = $1 AND state = 'reserved'"
        ))
        .bind(reservation.id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum AdmissionState {
    Pending,
    Active,
}

async fn lock_admission(connection: &mut PgConnection) -> Result<(), sqlx::Error> {
    sqlx::query(sql!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))"
    ))
    .bind(ADMISSION_LOCK)
    .execute(connection)
    .await?;
    Ok(())
}

/// `(organization, cluster)` counts of unexpired admissions in `state`.
async fn count_admissions(
    connection: &mut PgConnection,
    org_id: OrgId,
    state: AdmissionState,
) -> Result<(u64, u64), sqlx::Error> {
    let state = match state {
        AdmissionState::Pending => "pending",
        AdmissionState::Active => "active",
    };
    let (org, global) = sqlx::query_as::<_, (i64, i64)>(sql!(
        "SELECT
             COUNT(*) FILTER (WHERE org_id = $1),
             COUNT(*)
         FROM template_build_admissions
         WHERE state = $2 AND expires_at > NOW()"
    ))
    .bind(org_id.as_uuid())
    .bind(state)
    .fetch_one(connection)
    .await?;
    Ok((
        u64::try_from(org).unwrap_or(0),
        u64::try_from(global).unwrap_or(0),
    ))
}

fn check_count(
    limit: AdmissionLimit,
    org: u64,
    org_limit: u32,
    global: u64,
    global_limit: u32,
) -> Result<(), TemplateBuildAdmissionError> {
    if org >= u64::from(org_limit) {
        return Err(rejected(limit, AdmissionScope::Organization));
    }
    if global >= u64::from(global_limit) {
        return Err(rejected(limit, AdmissionScope::Global));
    }
    Ok(())
}

fn rejected(limit: AdmissionLimit, scope: AdmissionScope) -> TemplateBuildAdmissionError {
    pplx_space_metrics::template::build_admission(limit.resource(), scope.outcome());
    TemplateBuildAdmissionError::Rejected(AdmissionRejection { limit, scope })
}

fn admitted(resource: TemplateBuildAdmissionResource) {
    pplx_space_metrics::template::build_admission(
        resource,
        TemplateBuildAdmissionOutcome::Admitted,
    );
}

async fn settle_charge(
    connection: &mut PgConnection,
    reservation: InputReservation,
    size_bytes: u64,
    limits: &TemplateBuildLimits,
) -> Result<(), sqlx::Error> {
    sqlx::query(sql!(
        "UPDATE template_build_input_charges
         SET state = 'stored',
             size_bytes = $2,
             expires_at = NOW() + make_interval(secs => $3)
         WHERE id = $1"
    ))
    .bind(reservation.id)
    .bind(i64::try_from(size_bytes).unwrap_or(i64::MAX))
    .bind(limits.input_charge_window.as_secs_f64())
    .execute(connection)
    .await?;
    Ok(())
}

/// Retire state whose lifetime ended. Every admission sweeps first. Deleting a build's `template_build_jobs`
/// row cascades to its admission; deleting a mapping cascades to its
/// retention.
async fn sweep_expired(
    connection: &mut PgConnection,
    limits: &TemplateBuildLimits,
) -> Result<(), sqlx::Error> {
    let specs = sqlx::query(sql!(
        "DELETE FROM template_build_jobs jobs
         USING template_build_admissions admissions
         WHERE admissions.template_id = jobs.template_id
           AND admissions.build_id = jobs.build_id
           AND admissions.state = 'pending'
           AND admissions.expires_at <= NOW()"
    ))
    .execute(&mut *connection)
    .await?
    .rows_affected();
    pplx_space_metrics::template::build_gc(TemplateBuildGcKind::BuildSpec, specs);
    let records = sqlx::query(sql!(
        "DELETE FROM template_build_jobs jobs
         USING template_build_admissions admissions
         WHERE admissions.template_id = jobs.template_id
           AND admissions.build_id = jobs.build_id
           AND admissions.state = 'finished'
           AND admissions.expires_at <= NOW()"
    ))
    .execute(&mut *connection)
    .await?
    .rows_affected();
    pplx_space_metrics::template::build_gc(TemplateBuildGcKind::BuildRecord, records);
    // An active build past its lease no longer counts. Nobody observed its
    // end, so keep its record as long as a finished build's.
    let lapsed = sqlx::query(sql!(
        "UPDATE template_build_admissions
         SET state = 'finished',
             expires_at = NOW() + make_interval(secs => $1)
         WHERE state = 'active'
           AND expires_at <= NOW()"
    ))
    .bind(limits.build_record_retention.as_secs_f64())
    .execute(&mut *connection)
    .await?
    .rows_affected();
    if lapsed > 0 {
        tracing::warn!(
            builds = lapsed,
            "template builds outlived their admission lease without a terminal status observation"
        );
    }
    let mappings = sqlx::query(sql!(
        "DELETE FROM template_build_blobs blobs
         USING template_build_input_retention retention
         WHERE retention.template_id = blobs.template_id
           AND retention.files_hash = blobs.files_hash
           AND retention.expires_at <= NOW()"
    ))
    .execute(&mut *connection)
    .await?
    .rows_affected();
    pplx_space_metrics::template::build_gc(TemplateBuildGcKind::InputMapping, mappings);
    // A charge ends when its accounting window does, including one whose
    // upload never settled because the API died mid-upload. Ending a charge
    // deletes nothing stored; the bucket's lifecycle owns that.
    let charges = sqlx::query(sql!(
        "DELETE FROM template_build_input_charges WHERE expires_at <= NOW()"
    ))
    .execute(&mut *connection)
    .await?
    .rows_affected();
    pplx_space_metrics::template::build_gc(TemplateBuildGcKind::InputCharge, charges);
    Ok(())
}

#[cfg(test)]
mod tests;
