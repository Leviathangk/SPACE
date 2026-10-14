//! The gateway's execution identity and replacement epoch. Ingress and the
//! certificate authority share this owner without retaining each other's state.

use std::sync::{Arc, Mutex};

use pplx_space_common_svc::core::types::{
    ExecutionGeneration, NodeId, NodeManagedBy, PodUid, RouteGeneration,
    SandboxActivationAttemptId, SandboxId, SpaceApiRouteIdentity,
};
use pplx_space_common_svc::space_api_route::SpaceApiRouteIdentityDimension;
use tokio_util::sync::CancellationToken;
use tonic::Status;

use crate::proto::{SandboxSubstrateKind, SpaceApiRouteIdentity as ProtoSpaceApiRouteIdentity};

#[derive(Clone, Debug)]
pub struct ExecutionIdentity {
    trusted_sandbox_id: Option<SandboxId>,
    identity_epoch: Arc<Mutex<(Option<SpaceApiRouteIdentity>, CancellationToken)>>,
}

#[derive(Debug, thiserror::Error)]
#[error("execution identity state mutex poisoned")]
pub struct ReadExecutionIdentityError;

impl ExecutionIdentity {
    pub fn new(identity: Option<SpaceApiRouteIdentity>) -> Self {
        Self::new_inner(None, identity)
    }

    pub fn new_for_sandbox(
        trusted_sandbox_id: SandboxId,
        mut identity: Option<SpaceApiRouteIdentity>,
    ) -> Self {
        if let Some(identity) = identity.as_mut() {
            identity.sandbox_id = Some(trusted_sandbox_id);
        }
        Self::new_inner(Some(trusted_sandbox_id), identity)
    }

    fn new_inner(
        trusted_sandbox_id: Option<SandboxId>,
        identity: Option<SpaceApiRouteIdentity>,
    ) -> Self {
        Self {
            trusted_sandbox_id,
            identity_epoch: Arc::new(Mutex::new((identity, CancellationToken::new()))),
        }
    }

    pub fn update_identity(
        &self,
        mut identity: SpaceApiRouteIdentity,
    ) -> Result<RouteIdentityUpdate, RouteIdentityUpdateError> {
        if let Some(trusted_sandbox_id) = self.trusted_sandbox_id {
            if identity
                .sandbox_id
                .is_some_and(|sandbox_id| sandbox_id != trusted_sandbox_id)
            {
                return Err(RouteIdentityUpdateError::SandboxMismatch);
            }
            identity.sandbox_id = Some(trusted_sandbox_id);
        }
        let mut current = self
            .identity_epoch
            .lock()
            .map_err(|_| RouteIdentityUpdateError::StatePoisoned)?;
        let Some(incoming_generation) = identity.route_generation else {
            return Err(RouteIdentityUpdateError::MissingRouteGeneration);
        };

        let Some(existing) = current.0.as_ref() else {
            current.1.cancel();
            *current = (Some(identity), CancellationToken::new());
            return Ok(RouteIdentityUpdate::Updated);
        };
        let Some(existing_generation) = existing.route_generation else {
            current.1.cancel();
            *current = (Some(identity), CancellationToken::new());
            return Ok(RouteIdentityUpdate::Updated);
        };

        match incoming_generation.get().cmp(&existing_generation.get()) {
            std::cmp::Ordering::Less => {
                return Err(RouteIdentityUpdateError::StaleRouteGeneration {
                    current: existing_generation,
                    incoming: incoming_generation,
                });
            }
            std::cmp::Ordering::Greater => {
                current.1.cancel();
                *current = (Some(identity), CancellationToken::new());
                return Ok(RouteIdentityUpdate::Updated);
            }
            std::cmp::Ordering::Equal => {}
        }

        match merge_same_generation_identity(existing, &identity)? {
            SameGenerationMerge::Enriched => {
                current.1.cancel();
                *current = (Some(identity), CancellationToken::new());
                Ok(RouteIdentityUpdate::Updated)
            }
            SameGenerationMerge::AttemptRotated => {
                // The execution did not move — same generation, node,
                // substrate, and pod — so streams admitted under the previous
                // attempt stay valid; only new opens verify against the
                // rotated attempt.
                current.0 = Some(identity);
                Ok(RouteIdentityUpdate::Updated)
            }
            SameGenerationMerge::Unchanged => Ok(RouteIdentityUpdate::Unchanged),
        }
    }

    /// Snapshot the installed identity and an epoch cancelled on replacement.
    pub fn identity_with_epoch(
        &self,
    ) -> Result<(Option<SpaceApiRouteIdentity>, CancellationToken), ReadExecutionIdentityError>
    {
        self.identity_epoch
            .lock()
            .map(|epoch| (epoch.0.clone(), epoch.1.child_token()))
            .map_err(|_| ReadExecutionIdentityError)
    }

    #[cfg(test)]
    fn identity(&self) -> Option<SpaceApiRouteIdentity> {
        self.identity_with_epoch()
            .ok()
            .and_then(|(identity, _)| identity)
    }
}

impl Default for ExecutionIdentity {
    fn default() -> Self {
        Self::new(None)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RouteIdentityUpdate {
    Updated,
    Unchanged,
}

#[derive(Debug, thiserror::Error)]
pub enum RouteIdentityUpdateError {
    #[error("route identity state mutex poisoned")]
    StatePoisoned,
    #[error("route identity update requires route_generation")]
    MissingRouteGeneration,
    #[error("route identity targets a different sandbox")]
    SandboxMismatch,
    #[error(
        "stale route identity update: current route_generation={current}, incoming route_generation={incoming}"
    )]
    StaleRouteGeneration {
        current: RouteGeneration,
        incoming: RouteGeneration,
    },
    #[error("conflicting route identity update for route_generation={route_generation}: {field}")]
    Conflict {
        route_generation: RouteGeneration,
        field: SpaceApiRouteIdentityDimension,
    },
}

pub fn status_for_route_identity_update(error: RouteIdentityUpdateError) -> Status {
    match error {
        RouteIdentityUpdateError::StatePoisoned => Status::internal(error.to_string()),
        RouteIdentityUpdateError::MissingRouteGeneration => {
            Status::invalid_argument(error.to_string())
        }
        RouteIdentityUpdateError::SandboxMismatch
        | RouteIdentityUpdateError::StaleRouteGeneration { .. }
        | RouteIdentityUpdateError::Conflict { .. } => {
            Status::failed_precondition(error.to_string())
        }
    }
}

/// How an accepted same-generation identity update differs from the resident
/// identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SameGenerationMerge {
    Unchanged,
    /// A previously absent dimension was filled in. The identity became
    /// strictly more specific, so streams admitted against the weaker
    /// identity must not survive it.
    Enriched,
    /// Only the activation attempt was replaced. The control plane mints the
    /// activation attempt and rotates it on a wake whose lifecycle target is
    /// already satisfied; the runtime adopts the rotation and republishes it
    /// over this peer-authorized control socket, so the gateway has no basis
    /// to refuse it. Every dimension that describes where the execution
    /// lives stays immutable within a generation.
    AttemptRotated,
}

fn merge_same_generation_identity(
    current: &SpaceApiRouteIdentity,
    incoming: &SpaceApiRouteIdentity,
) -> Result<SameGenerationMerge, RouteIdentityUpdateError> {
    let route_generation = incoming
        .route_generation
        .expect("same-generation merge only runs after route_generation validation");
    let mut filled = false;
    filled |= merge_identity_field(
        route_generation,
        SpaceApiRouteIdentityDimension::SandboxId,
        current.sandbox_id.as_ref(),
        incoming.sandbox_id.as_ref(),
    )?;
    filled |= merge_identity_field(
        route_generation,
        SpaceApiRouteIdentityDimension::CurrentNodeId,
        current.current_node_id.as_ref(),
        incoming.current_node_id.as_ref(),
    )?;
    filled |= merge_identity_field(
        route_generation,
        SpaceApiRouteIdentityDimension::SubstrateKind,
        current.substrate_kind.as_ref(),
        incoming.substrate_kind.as_ref(),
    )?;
    filled |= merge_identity_field(
        route_generation,
        SpaceApiRouteIdentityDimension::ExecutionGeneration,
        current.execution_generation.as_ref(),
        incoming.execution_generation.as_ref(),
    )?;
    filled |= merge_identity_field(
        route_generation,
        SpaceApiRouteIdentityDimension::PodUid,
        current.pod_uid.as_ref(),
        incoming.pod_uid.as_ref(),
    )?;
    let attempt_rotated = match (
        current.activation_attempt_id.as_ref(),
        incoming.activation_attempt_id.as_ref(),
    ) {
        (Some(current), Some(incoming)) => current != incoming,
        (Some(_), None) => {
            return Err(RouteIdentityUpdateError::Conflict {
                route_generation,
                field: SpaceApiRouteIdentityDimension::ActivationAttemptId,
            });
        }
        (None, Some(_)) => {
            filled = true;
            false
        }
        (None, None) => false,
    };
    Ok(if filled {
        SameGenerationMerge::Enriched
    } else if attempt_rotated {
        SameGenerationMerge::AttemptRotated
    } else {
        SameGenerationMerge::Unchanged
    })
}

fn merge_identity_field<T: Eq>(
    route_generation: RouteGeneration,
    field: SpaceApiRouteIdentityDimension,
    current: Option<&T>,
    incoming: Option<&T>,
) -> Result<bool, RouteIdentityUpdateError> {
    match (current, incoming) {
        (Some(current), Some(incoming)) if current == incoming => Ok(false),
        (Some(_), Some(_)) | (Some(_), None) => Err(RouteIdentityUpdateError::Conflict {
            route_generation,
            field,
        }),
        (None, Some(_)) => Ok(true),
        (None, None) => Ok(false),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RouteIdentityParseError {
    #[error("sandbox_id is invalid")]
    SandboxId,
    #[error("route_generation is invalid")]
    RouteGeneration,
    #[error("current_node_id is invalid")]
    CurrentNodeId,
    #[error("execution_generation is invalid")]
    ExecutionGeneration,
    #[error("pod_uid is invalid")]
    PodUid,
    #[error("activation_attempt_id is invalid")]
    ActivationAttemptId,
    #[error("substrate_kind is invalid")]
    SubstrateKind,
}

pub fn route_identity_from_proto(
    identity: Option<ProtoSpaceApiRouteIdentity>,
) -> Result<Option<SpaceApiRouteIdentity>, RouteIdentityParseError> {
    let Some(identity) = identity else {
        return Ok(None);
    };
    Ok(Some(SpaceApiRouteIdentity {
        sandbox_id: identity
            .sandbox_id
            .as_deref()
            .map(str::parse::<SandboxId>)
            .transpose()
            .map_err(|_| RouteIdentityParseError::SandboxId)?,
        route_generation: identity
            .route_generation
            .map(RouteGeneration::try_from)
            .transpose()
            .map_err(|_| RouteIdentityParseError::RouteGeneration)?,
        current_node_id: identity
            .current_node_id
            .as_deref()
            .map(str::parse::<NodeId>)
            .transpose()
            .map_err(|_| RouteIdentityParseError::CurrentNodeId)?,
        substrate_kind: match SandboxSubstrateKind::try_from(identity.substrate_kind)
            .map_err(|_| RouteIdentityParseError::SubstrateKind)?
        {
            SandboxSubstrateKind::Unspecified => None,
            SandboxSubstrateKind::Direct => Some(NodeManagedBy::Space),
            SandboxSubstrateKind::Kubernetes => Some(NodeManagedBy::Kubernetes),
        },
        execution_generation: identity
            .execution_generation
            .map(|value| {
                ExecutionGeneration::new(value)
                    .map_err(|_| RouteIdentityParseError::ExecutionGeneration)
            })
            .transpose()?,
        pod_uid: identity
            .pod_uid
            .as_deref()
            .map(PodUid::parse)
            .transpose()
            .map_err(|_| RouteIdentityParseError::PodUid)?,
        activation_attempt_id: identity
            .activation_attempt_id
            .as_deref()
            .map(str::parse::<SandboxActivationAttemptId>)
            .transpose()
            .map_err(|_| RouteIdentityParseError::ActivationAttemptId)?,
    }))
}

pub fn route_identity_to_proto(
    identity: &SpaceApiRouteIdentity,
) -> Option<ProtoSpaceApiRouteIdentity> {
    (!identity.is_absent()).then(|| ProtoSpaceApiRouteIdentity {
        sandbox_id: identity.sandbox_id.map(|value| value.to_string()),
        route_generation: identity.route_generation.map(RouteGeneration::get),
        current_node_id: identity.current_node_id.map(|value| value.to_string()),
        substrate_kind: match identity.substrate_kind {
            None => SandboxSubstrateKind::Unspecified as i32,
            Some(NodeManagedBy::Space) => SandboxSubstrateKind::Direct as i32,
            Some(NodeManagedBy::Kubernetes) => SandboxSubstrateKind::Kubernetes as i32,
        },
        execution_generation: identity.execution_generation.map(ExecutionGeneration::get),
        pod_uid: identity.pod_uid.as_ref().map(ToString::to_string),
        activation_attempt_id: identity
            .activation_attempt_id
            .map(|value| value.to_string()),
    })
}

pub fn status_for_route_identity_parse(error: RouteIdentityParseError) -> Status {
    Status::invalid_argument(error.to_string())
}

#[cfg(test)]
mod tests;
