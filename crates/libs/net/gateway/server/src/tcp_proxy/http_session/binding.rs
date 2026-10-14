//! Policy and DNS attribution for the authority of an established HTTP connection.

use crate::dns::DnsAllowCache;
use crate::egress::{HostBindingError, authorize_host};
use crate::state::{GatewayNetworkState, TrafficPolicy};
use http::StatusCode;
use pplx_space_net_common::DomainName;
use std::net::SocketAddr;

#[derive(Clone)]
pub(in crate::tcp_proxy) struct HostBinding {
    sandbox_id: &'static str,
    destination: SocketAddr,
    network_state: GatewayNetworkState,
    dns_allow_cache: DnsAllowCache,
}

impl HostBinding {
    pub(in crate::tcp_proxy) fn new(
        sandbox_id: &'static str,
        destination: SocketAddr,
        network_state: GatewayNetworkState,
        dns_allow_cache: DnsAllowCache,
    ) -> Self {
        Self {
            sandbox_id,
            destination,
            network_state,
            dns_allow_cache,
        }
    }

    pub(super) fn sandbox_id(&self) -> &'static str {
        self.sandbox_id
    }

    pub(super) fn destination(&self) -> SocketAddr {
        self.destination
    }

    pub(super) async fn authorize(&self, host: &DomainName) -> Result<(), BindingOutcome> {
        // Established requests retain the bounded PendingPolicy grace.
        let policy = match self.network_state.wait_for_tcp_policy().await {
            TrafficPolicy::Ready(policy) => policy,
            TrafficPolicy::NotReady(_) => return Err(BindingOutcome::PolicyUnavailable),
        };
        authorize_host(&policy, self.destination, host, &self.dns_allow_cache).map_err(|error| {
            match error {
                HostBindingError::HostNotAllowed => BindingOutcome::HostNotAllowed,
                HostBindingError::HostUnattributed => BindingOutcome::HostUnattributed,
                HostBindingError::PolicyDenied(_) => BindingOutcome::PolicyDenied,
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BindingOutcome {
    Allowed,
    InvalidHost,
    InvalidHeaders,
    HostNotAllowed,
    HostUnattributed,
    PolicyDenied,
    PolicyUnavailable,
    UpstreamFailed,
    UpgradeUnsupported,
}

impl BindingOutcome {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::InvalidHost => "invalid_host",
            Self::InvalidHeaders => "invalid_headers",
            Self::HostNotAllowed => "host_not_allowed",
            Self::HostUnattributed => "host_unattributed",
            Self::PolicyDenied => "policy_denied",
            Self::PolicyUnavailable => "policy_unavailable",
            Self::UpstreamFailed => "upstream_failed",
            Self::UpgradeUnsupported => "upgrade_unsupported",
        }
    }

    pub(super) const fn refusal_status(self) -> StatusCode {
        match self {
            Self::InvalidHeaders => StatusCode::BAD_REQUEST,
            Self::PolicyUnavailable | Self::UpstreamFailed => StatusCode::BAD_GATEWAY,
            Self::Allowed
            | Self::InvalidHost
            | Self::HostNotAllowed
            | Self::HostUnattributed
            | Self::PolicyDenied
            | Self::UpgradeUnsupported => StatusCode::FORBIDDEN,
        }
    }
}
