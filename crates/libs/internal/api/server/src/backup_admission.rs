//! Authority to register an artifact backup, independent of worker inventory.
//!
//! A new artifact requires a current execution. A fresh attempt for a known
//! artifact inherits an exact earlier registration, even after retirement or
//! relocation. Kubernetes ownership always includes the recorded incarnation;
//! placement hints and today's registry cannot fill unknown historical UIDs.

use super::*;
use pplx_space_common_core::types::{K8sNodeUid, NodeManagedBy};

#[derive(Clone, Copy, Debug, Default)]
pub(super) enum OwnershipMode {
    #[default]
    Compatibility,
    Runtime,
    Strict,
}
#[derive(Debug, thiserror::Error)]
#[error("SPACE_BACKUP_OWNERSHIP_MODE must be compatibility, runtime, or strict")]
pub(super) struct InvalidOwnershipMode;
impl std::str::FromStr for OwnershipMode {
    type Err = InvalidOwnershipMode;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "compatibility" => Ok(Self::Compatibility),
            "runtime" => Ok(Self::Runtime),
            "strict" => Ok(Self::Strict),
            _ => Err(InvalidOwnershipMode),
        }
    }
}
impl std::fmt::Display for OwnershipMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
impl OwnershipMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compatibility => "compatibility",
            Self::Runtime => "runtime",
            Self::Strict => "strict",
        }
    }
    pub fn from_env() -> Result<Self, Status> {
        match std::env::var("SPACE_BACKUP_OWNERSHIP_MODE") {
            Err(std::env::VarError::NotPresent) => Ok(Self::default()),
            Ok(value) => value.parse().map_err(|error: InvalidOwnershipMode| {
                Status::failed_precondition(error.to_string())
            }),
            Err(_) => Err(Status::failed_precondition(
                InvalidOwnershipMode.to_string(),
            )),
        }
    }
    pub fn enforcing(self, flags: &pplx_space_feature_flags::FeatureFlagClient) -> bool {
        match self {
            Self::Compatibility => false,
            Self::Strict => true,
            Self::Runtime => {
                flags.bool_value(pplx_space_feature_flags::STRICT_BACKUP_OWNERSHIP, "cluster")
            }
        }
    }
}

/// Caller proof is resolved before acquiring database locks. An unavailable
/// verifier is unknown evidence, never authority inferred from today's registry.
#[derive(Clone, Debug)]
pub(super) struct BackupCaller {
    pub remote_addr: Option<std::net::SocketAddr>,
    pub node_uid: Result<K8sNodeUid, Status>,
    pub strict: bool,
}

#[cfg(test)]
#[derive(Clone)]
pub(super) struct TestVerifiedCaller(pub K8sNodeUid);

impl BackupCaller {
    pub async fn from_request<T>(request: &Request<T>, strict: bool) -> Self {
        #[cfg(test)]
        if let Some(identity) = request.extensions().get::<TestVerifiedCaller>() {
            return Self {
                remote_addr: request.remote_addr(),
                node_uid: Ok(identity.0.clone()),
                strict,
            };
        }
        Self {
            remote_addr: request.remote_addr(),
            strict,
            node_uid: auth::verify_node_bound_caller(request)
                .await
                .map(|identity| identity.node_uid().clone()),
        }
    }
}

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
    UnknownIdentity,
    ConflictingArtifact,
    WrongIncarnation,
}

impl Admission {
    pub fn source(&self) -> Option<&BackupSource> {
        match self {
            Self::Authorized(source) => Some(source),
            _ => None,
        }
    }

    pub fn decision(
        &self,
        caller: &BackupCaller,
    ) -> pplx_space_metrics::snapshot::BackupAdmissionDecision {
        use pplx_space_metrics::snapshot::BackupAdmissionDecision as Decision;
        match self {
            Self::Authorized(source) => {
                match (source.node_uid.as_ref(), caller.node_uid.as_ref()) {
                    (None, _) => Decision::Allowed,
                    (Some(expected), Ok(actual)) if expected == actual => Decision::Allowed,
                    (Some(_), Ok(_)) => Decision::PermanentlyRefused,
                    (Some(_), Err(_)) => Decision::InsufficientEvidence,
                }
            }
            Self::AwaitingExecution => Decision::Waiting,
            Self::UnknownIdentity => Decision::InsufficientEvidence,
            Self::UnregisteredArtifact | Self::ConflictingArtifact | Self::WrongIncarnation => {
                Decision::PermanentlyRefused
            }
        }
    }

    pub fn refusal(&self) -> Status {
        match self {
            Self::WrongIncarnation => {
                Status::permission_denied("backup belongs to another node incarnation")
            }
            Self::UnknownIdentity => {
                Status::failed_precondition("historical execution node identity is unknown")
            }
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

/// Inputs are fetched coherently by the caller. Registration holds the sandbox
/// and node locks; shadow evaluation uses a read-only repeatable-read snapshot.
pub(super) struct Evidence {
    present: bool,
    conflicting: bool,
    source_node: Option<NodeId>,
    prior: Option<snapshot_store::ArtifactRegistration>,
    node: Option<(NodeManagedBy, Option<K8sNodeUid>)>,
    execution_uid: Option<Option<K8sNodeUid>>,
    pending_execution: bool,
}

fn evaluate(evidence: Evidence) -> Admission {
    if !evidence.present {
        return Admission::UnregisteredArtifact;
    }
    if evidence.conflicting {
        return Admission::ConflictingArtifact;
    }
    let Some((managed_by, current_uid)) = evidence.node else {
        return Admission::UnregisteredArtifact;
    };
    let source = if let Some(prior) = evidence.prior {
        BackupSource {
            node_id: prior.node_id,
            node_uid: prior.node_uid,
            retry_of: Some(prior.snapshot_id),
        }
    } else {
        let Some(node_id) = evidence.source_node else {
            return Admission::UnregisteredArtifact;
        };
        let uid = if managed_by == NodeManagedBy::Kubernetes {
            match evidence.execution_uid {
                Some(Some(uid)) => Some(uid),
                Some(None) => return Admission::UnknownIdentity,
                None if evidence.pending_execution => return Admission::AwaitingExecution,
                None => return Admission::UnregisteredArtifact,
            }
        } else {
            None
        };
        BackupSource {
            node_id,
            node_uid: uid,
            retry_of: None,
        }
    };
    if managed_by == NodeManagedBy::Kubernetes && source.node_uid != current_uid {
        return Admission::WrongIncarnation;
    }
    Admission::Authorized(source)
}

#[derive(Clone, Copy)]
pub(super) enum ReadMode {
    Locked,
    Advisory,
}

pub(super) async fn source_node(
    conn: &mut sqlx::PgConnection,
    sandbox: SandboxId,
    artifact: &snapshot_store::SnapshotArtifact,
    caller_ip: Option<IpAddr>,
) -> Result<Admission, sqlx::Error> {
    Ok(evaluate(
        evidence(conn, sandbox, artifact, caller_ip, ReadMode::Locked).await?,
    ))
}

#[derive(Debug, serde::Serialize)]
pub struct BackupShadowRow {
    pub sandbox_id: SandboxId,
    pub artifact_id: Option<LocalArtifactId>,
    pub decision: &'static str,
    pub reason: String,
}
#[derive(Debug, serde::Serialize)]
pub struct BackupShadowPage {
    pub node_id: NodeId,
    pub rows: Vec<BackupShadowRow>,
    pub next_after: Option<SandboxId>,
    pub ready_pool_slots_without_uid: i64,
    pub active_executions_without_uid: i64,
    pub creating_attempts_without_uid: i64,
    pub complete_inventory_reported_at: Option<String>,
    /// A database read cannot prove a currently running daemon's TokenReview
    /// identity or completion of a fleet-wide writer rollout.
    pub live_caller_and_writer_coverage_required: bool,
}

mod store;
use store::evidence;
pub(super) use store::node_identity;
pub use store::shadow_page;
