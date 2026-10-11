//! Source authority for opening artifact backups.
//!
//! Placement is authoritative while present. After retirement, authority comes
//! from the last scheduled execution, not the node's census. The pending mirror
//! is an additional artifact-identity restriction: it may over-report, so its
//! presence never proves live ownership, file availability, or durability. A
//! stale report can admit an old artifact only from the same trusted execution
//! owner, with no active successor, restore lease, or conflicting current pin.
//! As with placed uploads, that authenticated worker supplies the artifact bytes.

use super::*;

/// Resolve and lock the source before binding a new attempt. Execution and
/// placement writers take the same sandbox lock, so their updates cannot
/// straddle these checks and the caller's snapshot insert.
pub(super) async fn source_node(
    conn: &mut sqlx::PgConnection,
    sandbox_id: SandboxId,
    artifact_id: LocalArtifactId,
    artifact_kind: ArtifactKind,
) -> Result<Option<NodeId>, sqlx::Error> {
    let placement: Option<(Option<NodeId>, Option<NodeId>)> = sqlx::query_as(sql!(
        "SELECT active_node_id, target_node_id FROM sandboxes WHERE id=$1 FOR UPDATE"
    ))
    .bind(sandbox_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((active, target)) = placement else {
        return Ok(None);
    };
    // Active placement wins; suspended reuploads retain their target node.
    if let Some(node) = active.or(target) {
        return Ok(Some(node));
    }
    sqlx::query_scalar(sql!(
        "SELECT pending.node_id
           FROM sandboxes sandbox
           JOIN node_k8s node ON node.k8s_node_name = (
               SELECT execution.k8s_node_name FROM sandbox_k8s_executions execution
                WHERE execution.sandbox_id = sandbox.id
                  AND execution.k8s_node_name IS NOT NULL
                ORDER BY execution.generation DESC LIMIT 1
           )
           JOIN nodes registered ON registered.id = node.node_id AND registered.role = 'worker'
           JOIN node_local_backup_pending pending
             ON pending.node_id = node.node_id AND pending.sandbox_id = sandbox.id
          WHERE sandbox.id = $1 AND sandbox.managed_by = 'k8s'
            AND sandbox.lifecycle IN ('suspended', 'suspending', 'stopped', 'stopping', 'restoring')
            AND (sandbox.restore_lease_expires_at IS NULL OR sandbox.restore_lease_expires_at <= NOW())
            AND (sandbox.current_local_artifact_id IS NULL OR sandbox.current_local_artifact_id = $2)
            AND pending.artifact_id = $2 AND pending.artifact_kind = $3
            AND NOT EXISTS (
                SELECT 1 FROM sandbox_k8s_executions current_execution
                 WHERE current_execution.sandbox_id = sandbox.id AND current_execution.active
            )"
    ))
    .bind(sandbox_id)
    .bind(artifact_id)
    .bind(artifact_kind.as_str())
    .fetch_optional(conn)
    .await
}
