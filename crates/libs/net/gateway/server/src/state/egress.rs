use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::info;

use super::{DnsAllowCache, DnsAttributedTcpTracker, GatewayNetworkState, NetworkPolicyV1};

/// Cancels reserved-DNS work that began before a network-policy install.
///
/// Adapters take a child token before reading the admitted policy. The policy
/// installation cancels that token before publishing the replacement policy,
/// so the guest retries the transaction against the new policy instead of
/// receiving a response authorized by the old one.
#[derive(Clone, Debug)]
pub struct ReservedDnsCancellation {
    current: Arc<parking_lot::Mutex<CancellationToken>>,
}

impl Default for ReservedDnsCancellation {
    fn default() -> Self {
        Self {
            current: Arc::new(parking_lot::Mutex::new(CancellationToken::new())),
        }
    }
}

impl ReservedDnsCancellation {
    pub fn operation_token(&self) -> CancellationToken {
        self.current.lock().child_token()
    }

    pub fn cancel_in_flight(&self) {
        let mut current = self.current.lock();
        current.cancel();
        *current = CancellationToken::new();
    }
}

impl GatewayNetworkState {
    /// Share the gateway's attribution and connection lifetimes before hydration.
    #[must_use]
    pub fn with_egress(
        mut self,
        cache: DnsAllowCache,
        connections: DnsAttributedTcpTracker,
        cancellation: ReservedDnsCancellation,
    ) -> Self {
        self.dns_allow_cache = cache;
        self.tcp_connections = connections;
        self.reserved_dns_cancellation = cancellation;
        self
    }

    pub(super) fn reconcile_policy(&self, policy: &NetworkPolicyV1) {
        self.reserved_dns_cancellation.cancel_in_flight();
        self.dns_allow_cache.retain_relevant_to_policy(policy);
        let cancelled = self
            .tcp_connections
            .cancel_denied(policy, &self.dns_allow_cache);
        if cancelled > 0 {
            info!(
                dns_attributed_cancelled_tcp_connections = cancelled,
                "reconciled TCP connections for installed network policy"
            );
        }
    }
}
