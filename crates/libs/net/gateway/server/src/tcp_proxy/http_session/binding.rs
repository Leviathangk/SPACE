//! Bind a request's hostname to the admitted destination under current policy.
//!
//! Both HTTP modes ask this one question before origin setup and again after
//! origin readiness: may this sandbox reach `host` at the admitted address?
use std::net::SocketAddr;
use std::sync::Arc;

use http::StatusCode;
use pplx_space_net_common::DomainName;

use super::super::upstream;
use crate::dns::DnsAllowCache;
use crate::egress::{HostBindingError, authorize_host};
use crate::state::{GatewayNetworkState, TrafficPolicy};

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

    pub(in crate::tcp_proxy) fn destination(&self) -> SocketAddr {
        self.destination
    }

    pub(in crate::tcp_proxy) async fn authorize(
        &self,
        host: &DomainName,
    ) -> Result<Arc<pplx_space_net_common::NetworkPolicyV1>, BindingOutcome> {
        // Established requests retain the bounded PendingPolicy grace.
        let policy = match self.network_state.wait_for_tcp_policy().await {
            TrafficPolicy::Ready(policy) => policy,
            TrafficPolicy::NotReady(_) => return Err(BindingOutcome::PolicyUnavailable),
        };
        authorize_host(&policy, self.destination, host, &self.dns_allow_cache).map_err(
            |error| match error {
                HostBindingError::HostNotAllowed => BindingOutcome::HostNotAllowed,
                HostBindingError::HostUnattributed => BindingOutcome::HostUnattributed,
                HostBindingError::PolicyDenied(_) => BindingOutcome::PolicyDenied,
            },
        )?;
        Ok(policy)
    }
    pub(in crate::tcp_proxy) async fn reauthorize(
        &self,
        host: &DomainName,
        policy: &Arc<pplx_space_net_common::NetworkPolicyV1>,
    ) -> Result<(), BindingOutcome> {
        if self.network_state.is_current_tcp_policy(policy) {
            return Ok(());
        }
        self.authorize(host).await.map(|_| ())
    }

    pub(in crate::tcp_proxy) fn record_setup_failure(&self, error: &upstream::Error) {
        let (failure, error_kind) = match error {
            upstream::Error::Connect(error) => ("connect", error.io_kind()),
            upstream::Error::Http(_) => ("http_handshake", None),
            upstream::Error::Tls(_) | upstream::Error::Cancelled => ("setup", None),
        };
        tracing::debug!(
            // The plaintext entry's event; keep its established target.
            target: "pplx_space_net_gateway::tcp_proxy::http_host",
            sandbox_id = self.sandbox_id,
            dst_port = self.destination.port(),
            address_family = if self.destination.is_ipv6() {
                "ipv6"
            } else {
                "ipv4"
            },
            failure,
            ?error_kind,
            "plaintext HTTP upstream connection failed"
        );
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::tcp_proxy) enum BindingOutcome {
    InvalidHost,
    InvalidHeaders,
    HostNotAllowed,
    HostUnattributed,
    PolicyDenied,
    PolicyUnavailable,
}

impl BindingOutcome {
    pub(in crate::tcp_proxy) const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidHost => "invalid_host",
            Self::InvalidHeaders => "invalid_headers",
            Self::HostNotAllowed => "host_not_allowed",
            Self::HostUnattributed => "host_unattributed",
            Self::PolicyDenied => "policy_denied",
            Self::PolicyUnavailable => "policy_unavailable",
        }
    }

    pub(in crate::tcp_proxy) const fn refusal_status(self) -> StatusCode {
        match self {
            Self::InvalidHeaders => StatusCode::BAD_REQUEST,
            Self::PolicyUnavailable => StatusCode::BAD_GATEWAY,
            Self::InvalidHost
            | Self::HostNotAllowed
            | Self::HostUnattributed
            | Self::PolicyDenied => StatusCode::FORBIDDEN,
        }
    }
}
