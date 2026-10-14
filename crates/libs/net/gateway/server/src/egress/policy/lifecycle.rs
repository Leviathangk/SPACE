use std::sync::Arc;
use std::time::Duration;

use pplx_space_net_common::{EffectiveNetworkPolicyRef, NetworkPolicyV1};

use super::credentials::NetworkCredentialSnapshot;
use super::source::{HydrateNetworkStateError, PulledNetworkPolicy};
use crate::proto::{NetworkPolicyErrorKind, NetworkState};

#[derive(Clone, Debug)]
pub(in crate::egress) enum PolicyLifecycle {
    NoIdentity,
    Pending {
        previous: Option<Arc<InstalledPolicy>>,
    },
    Ready(Arc<InstalledPolicy>),
    PolicyError(Arc<HydrationFailure>),
    Draining {
        previous: Option<Arc<InstalledPolicy>>,
    },
    Stopped,
}

impl PolicyLifecycle {
    pub(in crate::egress) fn installed(&self) -> Option<&Arc<InstalledPolicy>> {
        match self {
            Self::Ready(installed) => Some(installed),
            Self::Pending { previous } | Self::Draining { previous } => previous.as_ref(),
            Self::NoIdentity | Self::PolicyError(_) | Self::Stopped => None,
        }
    }

    pub(in crate::egress) fn ready(&self) -> Option<&Arc<InstalledPolicy>> {
        match self {
            Self::Ready(installed) => Some(installed),
            _ => None,
        }
    }

    pub(in crate::egress) fn failure(&self) -> Option<&HydrationFailure> {
        match self {
            Self::PolicyError(failure) => Some(failure),
            _ => None,
        }
    }

    pub(in crate::egress) fn mode(&self) -> NetworkState {
        match self {
            Self::NoIdentity => NetworkState::NoIdentity,
            Self::Pending { .. } => NetworkState::PendingPolicy,
            Self::Ready(_) => NetworkState::Ready,
            Self::PolicyError(_) => NetworkState::PolicyError,
            Self::Draining { .. } => NetworkState::Draining,
            Self::Stopped => NetworkState::Stopped,
        }
    }

    pub(in crate::egress) fn error_kind(&self) -> NetworkPolicyErrorKind {
        self.failure()
            .map_or(NetworkPolicyErrorKind::None, HydrationFailure::kind)
    }
}

#[derive(Debug)]
pub(in crate::egress) struct InstalledPolicy {
    // Each install allocates this Arc once. Existing traffic leases use its
    // identity to fence the complete bundle, including same-reference reloads.
    pub(in crate::egress) policy: Arc<NetworkPolicyV1>,
    pub(in crate::egress) credentials: NetworkCredentialSnapshot,
    pub(in crate::egress) reference: EffectiveNetworkPolicyRef,
}

impl From<PulledNetworkPolicy> for InstalledPolicy {
    fn from(pulled: PulledNetworkPolicy) -> Self {
        Self {
            policy: Arc::new(pulled.policy),
            credentials: pulled.network_credentials,
            reference: pulled.policy_ref,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(in crate::egress) enum HydrationFailure {
    #[error("policy ref mismatch: expected {expected}, got {actual}")]
    ReferenceMismatch {
        expected: EffectiveNetworkPolicyRef,
        actual: EffectiveNetworkPolicyRef,
    },
    #[error(transparent)]
    Load(#[from] HydrateNetworkStateError),
    #[error("policy hydration timed out after {} ms", .deadline.as_millis())]
    Timeout { deadline: Duration },
    #[error("policy hydration exhausted retry budget after {attempts} attempts")]
    RetryExhausted { attempts: u64 },
}

impl HydrationFailure {
    fn kind(&self) -> NetworkPolicyErrorKind {
        match self {
            Self::ReferenceMismatch { .. } => NetworkPolicyErrorKind::PolicyRefMismatch,
            Self::Load(_) | Self::RetryExhausted { .. } => NetworkPolicyErrorKind::LoaderError,
            Self::Timeout { .. } => NetworkPolicyErrorKind::Timeout,
        }
    }
}
