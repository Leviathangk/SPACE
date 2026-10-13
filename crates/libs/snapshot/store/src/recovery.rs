//! Explicit, bounded recovery authority. A grant proves an administrator's
//! approval of retained bytes, never their historical machine ownership.
use super::*;
use pplx_space_common_core::types::UserId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type, Serialize, Deserialize)]
#[sqlx(transparent)]
#[serde(transparent)]
pub struct RecoveryGrantId(Uuid);
impl RecoveryGrantId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }
}
impl Default for RecoveryGrantId {
    fn default() -> Self {
        Self::new()
    }
}
impl std::fmt::Display for RecoveryGrantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
impl FromStr for RecoveryGrantId {
    type Err = uuid::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self)
    }
}

/// Stable metadata proof excludes generated timestamps and measured sizes.
/// File proofs cover logical bytes read by volume-manager, not transport objects.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryManifest {
    pub execution: SnapshotExecutionMetadata,
    pub metadata_binding_digest: Digest,
    pub vcpu: i32,
    pub memory_mib: i32,
    pub template_id: TemplateName,
    pub content_hash: Option<String>,
    pub artifacts: Vec<SnapshotArtifactDigest>,
}
impl RecoveryManifest {
    pub fn validate(&self) -> Result<(), RecoveryError> {
        use SnapshotArtifactFile::*;
        let mut files: Vec<_> = self.artifacts.iter().map(|p| p.artifact).collect();
        files.sort();
        let valid = [
            vec![Rootfs, Memory, VmState],
            vec![RootfsBtrfsSend, Memory, VmState],
            vec![EncryptedSnapshotVolume, EncryptedRootfsCow],
        ]
        .into_iter()
        .any(|mut set| {
            set.sort();
            set == files
        });
        if !valid
            || self
                .artifacts
                .iter()
                .any(|p| p.size_bytes == 0 || p.size_bytes > i64::MAX as u64)
        {
            return Err(RecoveryError::Refused(
                "recovery requires an exact, nonempty full-artifact file manifest",
            ));
        }
        if self.vcpu <= 0 || self.memory_mib <= 0 {
            return Err(RecoveryError::Refused(
                "recovery requires the captured VM resource shape",
            ));
        }
        self.total_bytes()?;
        Ok(())
    }
    pub fn total_bytes(&self) -> Result<i64, RecoveryError> {
        self.artifacts
            .iter()
            .try_fold(0_i64, |sum, p| {
                i64::try_from(p.size_bytes)
                    .ok()
                    .and_then(|n| sum.checked_add(n))
            })
            .ok_or(RecoveryError::Refused(
                "recovery manifest size exceeds catalog range",
            ))
    }
    pub fn matches(
        &self,
        proofs: &[SnapshotArtifactDigest],
        metadata: Option<Digest>,
        bytes: i64,
    ) -> Result<bool, RecoveryError> {
        let mut expected = self.artifacts.clone();
        expected.sort_by_key(|p| p.artifact);
        let mut actual = proofs.to_vec();
        actual.sort_by_key(|p| p.artifact);
        Ok(expected == actual
            && metadata == Some(self.metadata_binding_digest)
            && bytes == self.total_bytes()?)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryError {
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    #[error(transparent)]
    Insert(#[from] InsertSnapshotError),
    #[error(transparent)]
    Read(#[from] GetSnapshotError),
    #[error("{0}")]
    Refused(&'static str),
}

pub struct ApproveRecovery<'a> {
    pub id: RecoveryGrantId,
    pub sandbox_id: SandboxId,
    pub artifact_id: LocalArtifactId,
    pub generation: ExecutionGeneration,
    pub uploader_node_id: NodeId,
    pub uploader_node_uid: &'a K8sNodeUid,
    pub captured_at: OffsetDateTime,
    pub expires_at: OffsetDateTime,
    pub actor: UserId,
    pub reason: &'a str,
    pub manifest: &'a RecoveryManifest,
}

#[derive(Debug, FromRow)]
pub struct RecoveryGrant {
    pub id: RecoveryGrantId,
    pub sandbox_id: SandboxId,
    pub local_artifact_id: LocalArtifactId,
    pub execution_generation: i64,
    pub captured_at: OffsetDateTime,
    pub uploader_node_id: NodeId,
    pub uploader_node_uid: K8sNodeUid,
    pub manifest: Json<RecoveryManifest>,
    pub expected_current_artifact_id: Option<LocalArtifactId>,
    pub active_attempt_id: Option<SnapshotId>,
    pub consumed_attempt_id: Option<SnapshotId>,
    pub usable: bool,
}

/// Caller must hold the sandbox row lock before acquiring grant or node locks.
/// The predicate is also checked at finalization, so approval is not a stale
/// authorization to restore across a successor execution.
async fn check_retained_state(
    conn: &mut sqlx::PgConnection,
    sandbox: SandboxId,
    generation: i64,
    artifact: LocalArtifactId,
    expected_pointer: Option<Option<LocalArtifactId>>,
) -> Result<Option<LocalArtifactId>, RecoveryError> {
    let row: Option<(Option<LocalArtifactId>, bool)> = sqlx::query_as(sql!(
        "SELECT current_local_artifact_id,
             active_node_id IS NULL AND lifecycle IN ('suspended','quarantined','stopped','failed')
             AND EXISTS (SELECT 1 FROM sandbox_k8s_executions e WHERE e.sandbox_id=s.id AND e.generation=$2)
             AND NOT EXISTS (SELECT 1 FROM sandbox_k8s_executions e WHERE e.sandbox_id=s.id AND (e.active OR e.generation>$2))
           FROM sandboxes s WHERE id=$1 FOR UPDATE"
    )).bind(sandbox).bind(generation).fetch_optional(&mut *conn).await?;
    let (pointer, eligible) = row.ok_or(RecoveryError::Refused("recovery sandbox is absent"))?;
    if !eligible
        || pointer.is_some_and(|id| id != artifact)
        || expected_pointer.is_some_and(|expected| expected != pointer)
    {
        return Err(RecoveryError::Refused(
            "recovery was superseded or the sandbox is not retired",
        ));
    }
    Ok(pointer)
}

async fn check_uploader(
    conn: &mut sqlx::PgConnection,
    node: NodeId,
    uid: &K8sNodeUid,
) -> Result<(), RecoveryError> {
    let eligible: Option<bool> = sqlx::query_scalar(sql!(
        "SELECT n.role='worker' AND n.managed_by='k8s' AND n.state<>'deleted' AND k.k8s_node_uid=$2
         FROM nodes n JOIN node_k8s k ON k.node_id=n.id WHERE n.id=$1 FOR SHARE OF n,k"
    ))
    .bind(node)
    .bind(uid)
    .fetch_optional(conn)
    .await?;
    if eligible != Some(true) {
        return Err(RecoveryError::Refused(
            "recovery uploader is not the approved live node incarnation",
        ));
    }
    Ok(())
}

/// Admin route owns authentication. Actor and exact approval are durable in the
/// same transaction; no worker-facing RPC can create or extend this authority.
pub async fn approve(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    request: ApproveRecovery<'_>,
) -> Result<(), RecoveryError> {
    request.manifest.validate()?;
    if request.reason.trim().is_empty() || request.reason.len() > 2048 {
        return Err(RecoveryError::Refused(
            "an approval reason of at most 2048 bytes is required",
        ));
    }
    let pointer = check_retained_state(
        tx,
        request.sandbox_id,
        request.generation.get(),
        request.artifact_id,
        None,
    )
    .await?;
    // Serialize grant replacement through the sandbox, including expired grants
    // whose accepted uploads may still be running. Only one approval is live.
    let occupied: bool = sqlx::query_scalar(sql!("SELECT EXISTS(SELECT 1 FROM backup_recovery_grants WHERE sandbox_id=$1 AND local_artifact_id=$2 AND revoked_at IS NULL AND consumed_attempt_id IS NULL AND expires_at>NOW())"))
        .bind(request.sandbox_id).bind(request.artifact_id).fetch_one(&mut **tx).await?;
    if occupied {
        return Err(RecoveryError::Refused(
            "an unexpired recovery approval already exists; revoke it before replacing it",
        ));
    }
    check_uploader(tx, request.uploader_node_id, request.uploader_node_uid).await?;
    let inserted = sqlx::query(sql!(
        "INSERT INTO backup_recovery_grants(id,sandbox_id,local_artifact_id,execution_generation,uploader_node_id,uploader_node_uid,captured_at,expires_at,approved_by,approval_reason,manifest,expected_current_artifact_id)
         SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12
         WHERE $7<=NOW() AND $8>NOW() AND $8<=NOW()+INTERVAL '24 hours'"
    )).bind(request.id).bind(request.sandbox_id).bind(request.artifact_id).bind(request.generation.get())
        .bind(request.uploader_node_id).bind(request.uploader_node_uid).bind(request.captured_at).bind(request.expires_at)
        .bind(request.actor).bind(request.reason.trim()).bind(Json(request.manifest)).bind(pointer).execute(&mut **tx).await?;
    if inserted.rows_affected() != 1 {
        return Err(RecoveryError::Refused(
            "capture must be in the past and approval must expire within 24 hours",
        ));
    }
    Ok(())
}

pub async fn revoke(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    sandbox: SandboxId,
    id: RecoveryGrantId,
    actor: UserId,
    reason: &str,
) -> Result<bool, RecoveryError> {
    if reason.trim().is_empty() || reason.len() > 2048 {
        return Err(RecoveryError::Refused(
            "a revocation reason of at most 2048 bytes is required",
        ));
    }
    sqlx::query(sql!("SELECT id FROM sandboxes WHERE id=$1 FOR UPDATE"))
        .bind(sandbox)
        .fetch_optional(&mut **tx)
        .await?;
    let result = sqlx::query(sql!("UPDATE backup_recovery_grants SET revoked_at=NOW(),revoked_by=$3,revoke_reason=$4 WHERE id=$1 AND sandbox_id=$2 AND revoked_at IS NULL AND consumed_attempt_id IS NULL"))
        .bind(id).bind(sandbox).bind(actor).bind(reason.trim()).execute(&mut **tx).await?;
    Ok(result.rows_affected() == 1)
}

pub async fn for_artifact(
    conn: &mut sqlx::PgConnection,
    sandbox: SandboxId,
    artifact: LocalArtifactId,
) -> Result<Option<RecoveryGrant>, RecoveryError> {
    Ok(sqlx::query_as(sql!("SELECT id,sandbox_id,local_artifact_id,execution_generation,captured_at,uploader_node_id,uploader_node_uid,manifest,expected_current_artifact_id,active_attempt_id,consumed_attempt_id,
        revoked_at IS NULL AND expires_at>NOW() AND consumed_attempt_id IS NULL AS usable
        FROM backup_recovery_grants WHERE sandbox_id=$1 AND local_artifact_id=$2 ORDER BY approved_at DESC,id DESC LIMIT 1 FOR UPDATE"))
        .bind(sandbox).bind(artifact).fetch_optional(conn).await?)
}

pub async fn for_attempt(
    conn: &mut sqlx::PgConnection,
    attempt: SnapshotId,
) -> Result<Option<RecoveryGrant>, RecoveryError> {
    Ok(sqlx::query_as(sql!("SELECT g.id,g.sandbox_id,g.local_artifact_id,g.execution_generation,g.captured_at,g.uploader_node_id,g.uploader_node_uid,g.manifest,g.expected_current_artifact_id,g.active_attempt_id,g.consumed_attempt_id,
        g.revoked_at IS NULL AND g.expires_at>NOW() AND g.consumed_attempt_id IS NULL AS usable
        FROM backup_recovery_grants g JOIN snapshots s ON s.recovery_grant_id=g.id WHERE s.id=$1 FOR UPDATE OF g"))
        .bind(attempt).fetch_optional(conn).await?)
}

impl RecoveryGrant {
    pub async fn validate_live(
        &self,
        conn: &mut sqlx::PgConnection,
        caller_uid: &K8sNodeUid,
    ) -> Result<(), RecoveryError> {
        if !self.usable {
            return Err(RecoveryError::Refused(
                "recovery approval is expired, revoked, or consumed",
            ));
        }
        if caller_uid != &self.uploader_node_uid {
            return Err(RecoveryError::Refused(
                "caller is not the approved recovery uploader",
            ));
        }
        check_retained_state(
            conn,
            self.sandbox_id,
            self.execution_generation,
            self.local_artifact_id,
            Some(self.expected_current_artifact_id),
        )
        .await?;
        let newer: bool = sqlx::query_scalar(sql!("SELECT EXISTS(SELECT 1 FROM snapshots WHERE sandbox_id=$1 AND local_artifact_id IS DISTINCT FROM $2 AND timestamp>$3 AND state<>'deleting')"))
            .bind(self.sandbox_id).bind(self.local_artifact_id).bind(self.captured_at).fetch_one(&mut *conn).await?;
        if newer {
            return Err(RecoveryError::Refused(
                "newer captured state supersedes this recovery",
            ));
        }
        check_uploader(conn, self.uploader_node_id, caller_uid).await
    }
}

/// Only this entry point can attach grant provenance to a fresh attempt.
/// Ordinary retry admission and store retry validation exclude all such rows.
pub async fn register(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    grant: &RecoveryGrant,
    mut snapshot: NewSnapshot,
    caller_uid: &K8sNodeUid,
) -> Result<SnapshotRecord, RecoveryError> {
    if caller_uid != &grant.uploader_node_uid {
        return Err(RecoveryError::Refused(
            "caller is not the approved recovery uploader",
        ));
    }
    let artifact = snapshot
        .artifact
        .as_ref()
        .ok_or(RecoveryError::Refused("recovery attempt has no artifact"))?;
    if snapshot.sandbox_id != grant.sandbox_id
        || artifact.local_artifact_id() != Some(grant.local_artifact_id)
        || artifact.kind() != ArtifactKind::Full
        || artifact.origin() != ArtifactOrigin::Suspend
        || artifact.snapshot_name().is_some()
        || snapshot
            .execution_metadata
            .as_ref()
            .map(SnapshotExecutionMetadata::execution)
            != Some(grant.manifest.execution.execution())
    {
        return Err(RecoveryError::Refused(
            "recovery attempt differs from the approved artifact or execution metadata",
        ));
    }
    if let Some(active) = grant.active_attempt_id.filter(|id| *id != snapshot.id) {
        let failed: Option<bool> = sqlx::query_scalar(sql!(
            "SELECT state='failed' FROM snapshots WHERE id=$1 FOR SHARE"
        ))
        .bind(active)
        .fetch_optional(&mut **tx)
        .await?;
        if failed != Some(true) {
            return Err(RecoveryError::Refused(
                "recovery already has an active attempt",
            ));
        }
    }
    snapshot.execution_metadata = Some(grant.manifest.execution.clone());
    snapshot.vcpu = Some(grant.manifest.vcpu);
    snapshot.memory_mib = Some(grant.manifest.memory_mib);
    snapshot.template_id = Some(grant.manifest.template_id.clone());
    snapshot.content_hash = grant
        .manifest
        .content_hash
        .clone()
        .map(SnapshotContentHash::from_stored);
    snapshot.source_node_id = Some(grant.uploader_node_id);
    snapshot.source_node_uid = Some(grant.uploader_node_uid.clone());
    snapshot.retry_of = None;
    snapshot.recovery_grant_id = Some(grant.id);
    if grant.consumed_attempt_id == Some(snapshot.id) {
        let persisted = get(&mut **tx, snapshot.id)
            .await?
            .ok_or(RecoveryError::Refused(
                "consumed recovery attempt is absent",
            ))?;
        if persisted.state().is_ready()
            && persisted.recovery_grant_id() == Some(grant.id)
            && same_snapshot_identity(&persisted, &snapshot)
        {
            return Ok(persisted);
        }
        return Err(RecoveryError::Refused(
            "consumed recovery identity conflicts with replay",
        ));
    }
    grant.validate_live(tx, caller_uid).await?;
    let result = insert(tx, &snapshot).await?;
    if result.recovery_grant_id() != Some(grant.id) {
        return Err(RecoveryError::Refused(
            "attempt is bound to another authority",
        ));
    }
    sqlx::query(sql!(
        "UPDATE snapshots SET generation=$2 WHERE id=$1 AND recovery_grant_id=$3"
    ))
    .bind(snapshot.id)
    .bind(grant.execution_generation)
    .bind(grant.id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(sql!(
        "UPDATE backup_recovery_grants SET active_attempt_id=$2 WHERE id=$1"
    ))
    .bind(grant.id)
    .bind(snapshot.id)
    .execute(&mut **tx)
    .await?;
    Ok(result)
}

/// Must commit atomically with Ready. Accepted upload work may finish after a
/// revoke, but it can never publish using revoked authority.
pub async fn consume(
    conn: &mut sqlx::PgConnection,
    grant: &RecoveryGrant,
    attempt: SnapshotId,
) -> Result<(), RecoveryError> {
    let result=sqlx::query(sql!("UPDATE backup_recovery_grants SET consumed_attempt_id=$2 WHERE id=$1 AND active_attempt_id=$2 AND consumed_attempt_id IS NULL AND revoked_at IS NULL AND expires_at>NOW()"))
        .bind(grant.id).bind(attempt).execute(conn).await?;
    if result.rows_affected() != 1 {
        return Err(RecoveryError::Refused(
            "recovery approval changed before finalization",
        ));
    }
    Ok(())
}

/// Read-only discovery after a lost admin response; audit records outlive attempts.
#[derive(Debug, FromRow, Serialize)]
pub struct RecoveryApprovalSummary {
    pub id: RecoveryGrantId,
    pub local_artifact_id: LocalArtifactId,
    pub execution_generation: i64,
    pub uploader_node_id: NodeId,
    pub uploader_node_uid: K8sNodeUid,
    pub approved_at: String,
    pub captured_at: String,
    pub expires_at: String,
    pub active_attempt_id: Option<SnapshotId>,
    pub consumed_attempt_id: Option<SnapshotId>,
    pub revoked: bool,
}
pub async fn list_approvals(
    conn: impl sqlx::PgExecutor<'_>,
    sandbox: SandboxId,
) -> Result<Vec<RecoveryApprovalSummary>, sqlx::Error> {
    sqlx::query_as(sql!("SELECT id,local_artifact_id,execution_generation,uploader_node_id,uploader_node_uid,approved_at::TEXT,captured_at::TEXT,expires_at::TEXT,active_attempt_id,consumed_attempt_id,revoked_at IS NOT NULL AS revoked FROM backup_recovery_grants WHERE sandbox_id=$1 ORDER BY approved_at DESC,id DESC LIMIT 100"))
        .bind(sandbox).fetch_all(conn).await
}
