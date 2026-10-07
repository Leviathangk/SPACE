//! Postgres-backed bookkeeping shared across requests.
//!
//! Three logical maps:
//! * `builds`: `(templateID, buildID) → BuildJob` — what `BuilderApi`
//!   job id the build was submitted under, so the SDK's status polls
//!   can resolve it to a buildmgr `GetBuildStatusRequest`.
//! * `uploaded_files`: `(templateID, filesHash) → blob_sha256` — what
//!   build-manager UploadBlob sha256 the file is stored under, so a
//!   `COPY` step in the e2b payload (referenced by `filesHash`) can be
//!   rewritten to a SPACE `Recipe.Step.Copy { blob_sha256 }` at translate
//!   time.
//! * `build_specs`: `(templateID, buildID) → BuildSpec` — sizing + tags
//!   posted to `POST /v3/templates` and the build id we synth'd back to
//!   the SDK. The SDK doesn't repeat these in the subsequent
//!   `POST /v2/templates/{tid}/builds/{bid}` call, so we stash them
//!   keyed by `(tid, bid)` (NOT `tid` alone — concurrent V3 calls for
//!   the same alias would otherwise clobber each other's tags or
//!   sizing).
//!
//! Each tenant write is admitted against per-organization and cluster-wide
//! limits, and expires; `template_build_admission_store` owns both.
use pplx_space_common_core::{
    sql,
    types::{OrgId, TemplateName},
};
use std::sync::Arc;

use sqlx::PgPool;

use crate::template_build_admission_store::{
    InputReservation, TemplateBuildAdmissionError, TemplateBuildAdmissionStore, TemplateBuildLimits,
};
pub(super) use crate::template_build_jobs_store::{BuildJob, BuildSpec};
use crate::template_build_jobs_store::{
    TemplateBuildJobsStore, TemplateBuildJobsStoreError, template_build_owner_matches,
};

pub(super) use crate::template_build_admission_store::RecordInputError as RecordBlobError;

pub struct TemplateBuildState {
    pool: PgPool,
    jobs: TemplateBuildJobsStore,
    admission: TemplateBuildAdmissionStore,
}

impl TemplateBuildState {
    pub fn new(pool: PgPool) -> Arc<Self> {
        Self::with_limits(pool, TemplateBuildLimits::default())
    }

    pub fn with_limits(pool: PgPool, limits: TemplateBuildLimits) -> Arc<Self> {
        Arc::new(Self {
            jobs: TemplateBuildJobsStore::new(pool.clone()),
            admission: TemplateBuildAdmissionStore::new(pool.clone(), limits),
            pool,
        })
    }

    pub(super) fn limits(&self) -> &TemplateBuildLimits {
        self.admission.limits()
    }

    pub(super) async fn lookup_build(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        build_id: &str,
    ) -> Result<Option<BuildJob>, TemplateBuildJobsStoreError> {
        self.jobs
            .lookup_build(org_id, template_name, build_id)
            .await
    }

    pub(super) async fn reserve_upload(
        &self,
        org_id: OrgId,
    ) -> Result<InputReservation, TemplateBuildAdmissionError> {
        self.admission.reserve_input_upload(org_id).await
    }

    pub(super) async fn template_owner_matches(
        &self,
        template_name: &TemplateName,
        org_id: OrgId,
    ) -> Result<bool, sqlx::Error> {
        template_build_owner_matches(&self.pool, template_name, org_id).await
    }

    pub(super) async fn settle_upload(
        &self,
        reservation: InputReservation,
        size_bytes: u64,
    ) -> Result<(), sqlx::Error> {
        self.admission.settle_input(reservation, size_bytes).await
    }

    pub(super) async fn release_upload(&self, reservation: InputReservation) {
        if let Err(error) = self.admission.release_input_reservation(reservation).await {
            tracing::warn!(
                %error,
                "failed to release a template file upload reservation; it expires on its own"
            );
        }
    }

    pub(super) async fn record_blob(
        &self,
        reservation: InputReservation,
        org_id: OrgId,
        template_name: &TemplateName,
        files_hash: &str,
        blob_sha256: &str,
        size_bytes: u64,
    ) -> Result<(), RecordBlobError> {
        self.admission
            .record_input(
                reservation,
                org_id,
                template_name,
                files_hash,
                blob_sha256,
                size_bytes,
            )
            .await
    }

    /// The blob an unexpired upload mapping names. A mapping recorded before
    /// retention existed has no retention row and is honoured for the same
    /// lifetime from its own creation; past it, the shared object may already
    /// be gone.
    pub(super) async fn lookup_blob(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        files_hash: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        if !template_build_owner_matches(&self.pool, template_name, org_id).await? {
            return Ok(None);
        }
        sqlx::query_scalar::<_, String>(sql!(
            r#"
            SELECT blobs.blob_sha256
            FROM template_build_blobs blobs
            LEFT JOIN template_build_input_retention retention
              ON retention.template_id = blobs.template_id
             AND retention.files_hash = blobs.files_hash
            WHERE blobs.template_id = $1
              AND blobs.files_hash = $2
              AND (blobs.org_id = $3 OR blobs.org_id IS NULL)
              AND COALESCE(
                      retention.expires_at,
                      blobs.created_at + make_interval(secs => $4)
                  ) > NOW()
            "#
        ))
        .bind(template_name.as_str())
        .bind(files_hash)
        .bind(org_id.as_uuid())
        .bind(self.limits().input_mapping_ttl.as_secs_f64())
        .fetch_optional(&self.pool)
        .await
    }

    pub(super) async fn record_build_spec(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        build_id: &str,
        spec: BuildSpec,
    ) -> Result<(), TemplateBuildAdmissionError> {
        self.admission
            .admit_build_spec(org_id, template_name, build_id, spec)
            .await
    }

    pub(super) async fn lookup_build_spec(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        build_id: &str,
    ) -> Result<Option<BuildSpec>, TemplateBuildJobsStoreError> {
        self.jobs
            .lookup_build_spec(org_id, template_name, build_id)
            .await
    }

    pub(super) async fn admit_build(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        build_id: &str,
    ) -> Result<(), TemplateBuildAdmissionError> {
        self.admission
            .admit_build(org_id, template_name, build_id)
            .await
    }

    pub(super) async fn release_build(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        build_id: &str,
    ) {
        if let Err(error) = self
            .admission
            .release_build(org_id, template_name, build_id)
            .await
        {
            tracing::warn!(
                %error,
                %template_name,
                "failed to release an undispatched template build admission; it expires on its own"
            );
        }
    }

    pub(super) async fn finish_build(
        &self,
        org_id: OrgId,
        template_name: &TemplateName,
        build_id: &str,
    ) {
        if let Err(error) = self
            .admission
            .finish_build(org_id, template_name, build_id)
            .await
        {
            tracing::warn!(
                %error,
                %template_name,
                "failed to record a finished template build; its admission expires on its own"
            );
        }
    }
}

#[cfg(test)]
mod tests;
