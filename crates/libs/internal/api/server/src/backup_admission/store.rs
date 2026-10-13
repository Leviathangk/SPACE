//! Database evidence for backup admission and advisory rollout diagnostics.
use super::*;

#[derive(sqlx::FromRow)]
struct PlacementEvidence {
    active_node_id: Option<NodeId>,
    target_node_id: Option<NodeId>,
    current_local_artifact_id: Option<LocalArtifactId>,
    current_local_artifact_kind: Option<String>,
    current_local_artifact_origin: Option<String>,
}

#[derive(sqlx::FromRow)]
struct ShadowCandidate {
    sandbox_id: SandboxId,
    artifact_id: Option<LocalArtifactId>,
    artifact_kind: Option<String>,
    artifact_origin: Option<String>,
}

pub(super) async fn evidence(
    conn: &mut sqlx::PgConnection,
    sandbox: SandboxId,
    artifact: &snapshot_store::SnapshotArtifact,
    caller_ip: Option<IpAddr>,
    mode: ReadMode,
) -> Result<Evidence, sqlx::Error> {
    // Explicit complete statements preserve the locking contract. The shared
    // evaluator below owns decisions, never the data-fetch mode.
    let placement: Option<PlacementEvidence> = match mode {
        ReadMode::Locked => sqlx::query_as(sql!("SELECT active_node_id,target_node_id,current_local_artifact_id,current_local_artifact_kind,current_local_artifact_origin FROM sandboxes WHERE id=$1 FOR UPDATE")).bind(sandbox).fetch_optional(&mut *conn).await?,
        ReadMode::Advisory => sqlx::query_as(sql!("SELECT active_node_id,target_node_id,current_local_artifact_id,current_local_artifact_kind,current_local_artifact_origin FROM sandboxes WHERE id=$1")).bind(sandbox).fetch_optional(&mut *conn).await?,
    };
    let present = placement.is_some();
    let pin_conflict = placement.as_ref().is_some_and(|row| {
        row.current_local_artifact_id.is_some()
            && row.current_local_artifact_id == artifact.local_artifact_id()
            && (row.current_local_artifact_kind.as_deref() != Some(artifact.kind().as_str())
                || row.current_local_artifact_origin.as_deref() != Some(artifact.origin().as_str())
                || artifact.snapshot_name().is_some())
    });
    let conflicting = pin_conflict
        || snapshot_store::artifact_provenance_conflicts(&mut *conn, sandbox, artifact).await?;
    let prior =
        snapshot_store::earliest_registered_artifact_for_caller(conn, sandbox, artifact, caller_ip)
            .await?;
    // Prior registration wins over placement; active placement wins over target.
    let source_node = prior.as_ref().map(|p| p.node_id).or_else(|| {
        placement
            .as_ref()
            .and_then(|row| row.active_node_id.or(row.target_node_id))
    });
    let node = if let Some(node) = source_node {
        match mode {
            ReadMode::Locked => node_identity(conn,node).await?,
            ReadMode::Advisory => sqlx::query_as(sql!("SELECT n.managed_by,NULLIF(BTRIM(k.k8s_node_uid),'') FROM nodes n LEFT JOIN node_k8s k ON k.node_id=n.id WHERE n.id=$1 AND n.role='worker' AND n.state<>'deleted'"))
                .bind(node).fetch_optional(&mut *conn).await?,
        }
    } else {
        None
    };
    let execution_uid=sqlx::query_scalar(sql!("SELECT NULLIF(BTRIM(e.k8s_node_uid),'') FROM sandbox_k8s_executions e JOIN node_k8s k ON k.node_id=$2 WHERE e.sandbox_id=$1 AND e.active AND e.k8s_node_name=k.k8s_node_name"))
        .bind(sandbox).bind(source_node).fetch_optional(&mut *conn).await?;
    let pending_execution=sqlx::query_scalar(sql!("SELECT EXISTS(SELECT 1 FROM sandbox_k8s_executions WHERE sandbox_id=$1 AND active AND k8s_node_name IS NULL)"))
        .bind(sandbox).fetch_one(&mut *conn).await?;
    Ok(Evidence {
        present,
        conflicting,
        source_node,
        prior,
        node,
        execution_uid,
        pending_execution,
    })
}

pub(crate) async fn node_identity(
    conn: &mut sqlx::PgConnection,
    node_id: NodeId,
) -> Result<Option<(NodeManagedBy, Option<K8sNodeUid>)>, sqlx::Error> {
    let managed_by: Option<NodeManagedBy> = sqlx::query_scalar(sql!(
        "SELECT managed_by FROM nodes WHERE id=$1 AND role='worker' AND state<>'deleted' FOR SHARE"
    ))
    .bind(node_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(managed_by) = managed_by else {
        return Ok(None);
    };
    let uid: Option<Option<K8sNodeUid>> = sqlx::query_scalar(sql!(
        "SELECT NULLIF(BTRIM(k8s_node_uid), '') FROM node_k8s WHERE node_id=$1 FOR SHARE"
    ))
    .bind(node_id)
    .fetch_optional(conn)
    .await?;
    Ok(Some((managed_by, uid.flatten())))
}

/// One bounded advisory page, with a coherent database snapshot and no row
/// locks. Includes drain blockers and inventory-only retained artifacts. Missing
/// artifact identity is reported as unknown rather than silently omitted.
/// Pages are individually coherent; the result is never an authorization token.
pub async fn shadow_page(
    pool: &sqlx::PgPool,
    node: NodeId,
    after: Option<SandboxId>,
    limit: u32,
) -> Result<BackupShadowPage, sqlx::Error> {
    let limit = limit.clamp(1, 100);
    let mut tx = pool
        .begin_with(sql!("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY"))
        .await?;
    sqlx::query(sql!("SET LOCAL statement_timeout='5s'"))
        .execute(&mut *tx)
        .await?;
    let blockers = pplx_space_sandbox_portability::drain_gate_blockers_for_node("$1");
    let query=format!("SELECT s.id AS sandbox_id,COALESCE(p.artifact_id,s.current_local_artifact_id) AS artifact_id,
        CASE WHEN p.artifact_id IS NULL OR p.artifact_id=s.current_local_artifact_id THEN s.current_local_artifact_kind ELSE p.artifact_kind END AS artifact_kind,
        CASE WHEN p.artifact_id IS NULL OR p.artifact_id=s.current_local_artifact_id THEN s.current_local_artifact_origin ELSE NULL END AS artifact_origin
        FROM sandboxes s LEFT JOIN node_local_backup_pending p ON p.node_id=$1 AND p.sandbox_id=s.id WHERE ($2::UUID IS NULL OR s.id>$2) AND s.id IN (
            {blockers}
            UNION SELECT sandbox_id FROM node_local_backup_pending WHERE node_id=$1
            UNION SELECT e.sandbox_id FROM sandbox_k8s_executions e JOIN node_k8s k ON k.k8s_node_name=e.k8s_node_name WHERE k.node_id=$1 AND e.active AND e.k8s_node_uid IS NULL
            UNION SELECT sandbox_id FROM snapshots WHERE source_node_id=$1 AND state='creating' AND source_node_uid IS NULL
        ) ORDER BY s.id LIMIT $3");
    let mut candidates: Vec<ShadowCandidate> = sqlx::query_as(sqlx::AssertSqlSafe(sql!(query)))
        .bind(node)
        .bind(after)
        .bind(i64::from(limit) + 1)
        .fetch_all(&mut *tx)
        .await?;
    let more = candidates.len() > limit as usize;
    candidates.truncate(limit as usize);
    let address: Option<String> =
        sqlx::query_scalar(sql!("SELECT host(address) FROM nodes WHERE id=$1"))
            .bind(node)
            .fetch_optional(&mut *tx)
            .await?
            .flatten();
    let ip = address.and_then(|value| value.parse().ok());
    let unknown_caller = BackupCaller {
        remote_addr: ip.map(|ip| std::net::SocketAddr::new(ip, 0)),
        node_uid: Err(Status::unavailable(
            "shadow reads cannot prove live caller identity",
        )),
        strict: true,
    };
    let mut rows = Vec::with_capacity(candidates.len());
    for ShadowCandidate {
        sandbox_id: sandbox,
        artifact_id: artifact,
        artifact_kind: kind,
        artifact_origin: origin,
    } in candidates
    {
        let identity = match (artifact, kind.as_deref(), origin.as_deref()) {
            (Some(id), Some(kind), Some(origin)) => ArtifactKind::from_str(kind)
                .ok()
                .zip(ArtifactOrigin::from_str(origin).ok())
                .and_then(|(kind, origin)| {
                    snapshot_store::SnapshotArtifact::new(Some(id), kind, origin, None).ok()
                }),
            _ => None,
        };
        let (decision, reason) = if let Some(identity) = identity {
            let admission =
                evaluate(evidence(&mut tx, sandbox, &identity, ip, ReadMode::Advisory).await?);
            (
                admission.decision(&unknown_caller).as_str(),
                format!("{admission:?}"),
            )
        } else {
            (
                "insufficient_evidence",
                "retained artifact identity is absent or inconsistent".to_owned(),
            )
        };
        rows.push(BackupShadowRow {
            sandbox_id: sandbox,
            artifact_id: artifact,
            decision,
            reason,
        });
    }
    let ready_pool_slots_without_uid=sqlx::query_scalar(sql!("SELECT COUNT(*) FROM kubernetes_pool_slots p JOIN node_k8s k ON k.k8s_node_name=p.k8s_node_name WHERE k.node_id=$1 AND p.state='ready' AND p.k8s_node_uid IS NULL")).bind(node).fetch_one(&mut *tx).await?;
    let active_executions_without_uid=sqlx::query_scalar(sql!("SELECT COUNT(*) FROM sandbox_k8s_executions e JOIN node_k8s k ON k.k8s_node_name=e.k8s_node_name WHERE k.node_id=$1 AND e.active AND e.k8s_node_uid IS NULL")).bind(node).fetch_one(&mut *tx).await?;
    let creating_attempts_without_uid=sqlx::query_scalar(sql!("SELECT COUNT(*) FROM snapshots WHERE source_node_id=$1 AND state='creating' AND source_node_uid IS NULL")).bind(node).fetch_one(&mut *tx).await?;
    let complete_inventory_reported_at=sqlx::query_scalar(sql!("SELECT local_backup_inventory_reported_at::TEXT FROM node_status_snapshots WHERE node_id=$1")).bind(node).fetch_optional(&mut *tx).await?.flatten();
    tx.commit().await?;
    Ok(BackupShadowPage {
        node_id: node,
        next_after: more.then(|| rows.last().expect("positive page limit").sandbox_id),
        rows,
        ready_pool_slots_without_uid,
        active_executions_without_uid,
        creating_attempts_without_uid,
        complete_inventory_reported_at,
        live_caller_and_writer_coverage_required: true,
    })
}
