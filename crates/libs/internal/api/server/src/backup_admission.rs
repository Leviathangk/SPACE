//! Authority to register an artifact backup, independent of worker inventory.
//!
//! A new artifact requires a current execution. A fresh attempt for a known
//! artifact inherits an exact earlier registration, even after retirement or
//! relocation. Kubernetes ownership always includes the recorded incarnation;
//! placement hints and today's registry cannot fill unknown historical UIDs.

use super::*;
use pplx_space_common_core::types::{K8sNodeUid, NodeManagedBy};

#[derive(Debug)]
pub(super) struct BackupSource {
    pub node_id: NodeId,
    pub node_uid: Option<K8sNodeUid>,
    pub retry_of: Option<SnapshotId>,
}

#[derive(Debug)]
pub(super) enum Admission {
    Authorized(BackupSource),
    AwaitingExecution,
    UnregisteredArtifact,
    ConflictingArtifact,
}

impl Admission {
    pub fn source(&self) -> Option<&BackupSource> {
        match self {
            Self::Authorized(source) => Some(source),
            _ => None,
        }
    }

    pub fn refusal(&self) -> Status {
        match self {
            Self::AwaitingExecution => Status::unavailable(
                "backup source is waiting for a current execution with recorded node identity",
            ),
            Self::UnregisteredArtifact => Status::failed_precondition(
                "first backup registration requires a current execution; retired artifacts require an earlier verified registration",
            ),
            Self::ConflictingArtifact => Status::failed_precondition(
                "artifact kind, origin, or name conflicts with recorded provenance",
            ),
            Self::Authorized(_) => Status::internal("authorized backup lost its source"),
        }
    }
}

/// The sandbox lock serializes registration with execution changes and artifact
/// retention. Node locks are acquired afterwards, in registry-writer order.
pub(super) async fn source_node(
    conn: &mut sqlx::PgConnection,
    sandbox_id: SandboxId,
    artifact: &snapshot_store::SnapshotArtifact,
    caller_ip: Option<IpAddr>,
) -> Result<Admission, sqlx::Error> {
    let placement: Option<(Option<NodeId>, Option<NodeId>)> = sqlx::query_as(sql!(
        "SELECT active_node_id, target_node_id FROM sandboxes WHERE id=$1 FOR UPDATE"
    ))
    .bind(sandbox_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((active, target)) = placement else {
        return Ok(Admission::UnregisteredArtifact);
    };
    let conflicting: bool = sqlx::query_scalar(sql!(
        "SELECT EXISTS (
             SELECT 1 FROM snapshots WHERE sandbox_id=$1 AND local_artifact_id=$2
               AND (artifact_kind IS DISTINCT FROM $3 OR artifact_origin IS DISTINCT FROM $4
                    OR artifact_snapshot_name IS DISTINCT FROM $5)
         ) OR EXISTS (
             SELECT 1 FROM sandboxes WHERE id=$1 AND current_local_artifact_id=$2
               AND (current_local_artifact_kind IS DISTINCT FROM $3
                    OR current_local_artifact_origin IS DISTINCT FROM $4 OR $5::TEXT IS NOT NULL)
         )"
    ))
    .bind(sandbox_id)
    .bind(artifact.local_artifact_id())
    .bind(artifact.kind().as_str())
    .bind(artifact.origin().as_str())
    .bind(artifact.snapshot_name().map(|name| name.as_str()))
    .fetch_one(&mut *conn)
    .await?;
    if conflicting {
        return Ok(Admission::ConflictingArtifact);
    }

    // Inventory, current placement, and later executions cannot grant or revoke
    // an already registered artifact's ownership. A deleting catalog row is not
    // a retry grant; once all registration evidence is pruned this fails closed.
    let prior: Option<(SnapshotId, NodeId, Option<K8sNodeUid>)> = sqlx::query_as(sql!(
        "SELECT prior.id, prior.source_node_id, prior.source_node_uid
           FROM snapshots prior JOIN nodes owner ON owner.id=prior.source_node_id
          WHERE prior.sandbox_id=$1 AND prior.local_artifact_id=$2
            AND prior.artifact_kind=$3 AND prior.artifact_origin=$4
            AND prior.artifact_snapshot_name IS NOT DISTINCT FROM $5
            AND prior.state IN ('creating','failed','ready')
            AND owner.role='worker' AND host(owner.address)::INET IN ($6::INET, $7::INET)
            AND (owner.managed_by <> 'k8s' OR prior.source_node_uid IS NOT NULL)
          ORDER BY prior.timestamp ASC, prior.id ASC LIMIT 1"
    ))
    .bind(sandbox_id)
    .bind(artifact.local_artifact_id())
    .bind(artifact.kind().as_str())
    .bind(artifact.origin().as_str())
    .bind(artifact.snapshot_name().map(|name| name.as_str()))
    .bind(caller_ip.map(normalize_ip).map(|ip| ip.to_string()))
    .bind(caller_ip.map(normalize_ip).map(|ip| match ip {
        IpAddr::V4(ip) => ip.to_ipv6_mapped().to_string(),
        IpAddr::V6(ip) => ip.to_string(),
    }))
    .fetch_optional(&mut *conn)
    .await?;
    if let Some((prior_id, node_id, node_uid)) = prior {
        return Ok(Admission::Authorized(BackupSource {
            node_id,
            node_uid,
            retry_of: Some(prior_id),
        }));
    }

    // Active placement wins over the target hint. Neither alone proves a
    // Kubernetes execution's incarnation, including target-only placement.
    let Some(node_id) = active.or(target) else {
        return Ok(Admission::UnregisteredArtifact);
    };
    let Some((managed_by, _current_uid)) = node_identity(conn, node_id).await? else {
        return Ok(Admission::AwaitingExecution);
    };
    if managed_by == NodeManagedBy::Kubernetes {
        let execution: Option<Option<K8sNodeUid>> = sqlx::query_scalar(sql!(
            "SELECT NULLIF(BTRIM(execution.k8s_node_uid), '')
               FROM sandbox_k8s_executions execution
               JOIN node_k8s node ON node.node_id=$2
              WHERE execution.sandbox_id=$1 AND execution.active
                AND execution.k8s_node_name=node.k8s_node_name"
        ))
        .bind(sandbox_id)
        .bind(node_id)
        .fetch_optional(&mut *conn)
        .await?;
        let Some(recorded_uid) = execution.flatten() else {
            // An active, unscheduled execution may still acquire its first
            // binding. Historical names without UIDs cannot be backfilled.
            let pending: bool = sqlx::query_scalar(sql!(
                "SELECT EXISTS (SELECT 1 FROM sandbox_k8s_executions
                  WHERE sandbox_id=$1 AND active AND k8s_node_name IS NULL)"
            ))
            .bind(sandbox_id)
            .fetch_one(&mut *conn)
            .await?;
            return Ok(if pending {
                Admission::AwaitingExecution
            } else {
                Admission::UnregisteredArtifact
            });
        };
        // Preserve the execution's UID even on mismatch: the shared authorizer
        // refuses it against the locked registry, just like replay/finalize.
        return Ok(Admission::Authorized(BackupSource {
            node_id,
            node_uid: Some(recorded_uid),
            retry_of: None,
        }));
    }
    // Direct workers have no Kubernetes incarnation. Their existing registered
    // node/address authority remains the explicit compatibility policy.
    Ok(Admission::Authorized(BackupSource {
        node_id,
        node_uid: None,
        retry_of: None,
    }))
}

pub(super) async fn node_identity(
    conn: &mut sqlx::PgConnection,
    node_id: NodeId,
) -> Result<Option<(NodeManagedBy, Option<K8sNodeUid>)>, sqlx::Error> {
    let managed_by: Option<NodeManagedBy> = sqlx::query_scalar(sql!(
        "SELECT managed_by FROM nodes WHERE id=$1 AND role='worker' FOR SHARE"
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
