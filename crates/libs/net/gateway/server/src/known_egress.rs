use std::net::SocketAddr;
use std::sync::Arc;

use hickory_resolver::proto::op::Message;
use pplx_space_net_common::DomainName;
use tracing::{debug, error, info};

/// The relay peer whose socket failed after the upstream connect succeeded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TransferPeer {
    Guest,
    Destination,
}

/// Diagnostic logging for failed egress to configured domains. A DNS query or
/// TCP flow that never gets a working upstream logs an error; a TCP flow that
/// fails after connecting is logged below error.
#[derive(Clone, Debug, Default)]
pub struct KnownEgressState {
    domains: Arc<[DomainName]>,
}

impl KnownEgressState {
    pub fn new(domains: Vec<DomainName>) -> Self {
        Self {
            domains: domains.into(),
        }
    }

    /// A TCP flow that never had a working upstream connection: refused
    /// before relaying, or the connect itself failed.
    pub(crate) fn report_tcp_failure(
        &self,
        sandbox_id: &str,
        original_dst: SocketAddr,
        attempted_host: &DomainName,
        reason: &str,
    ) -> bool {
        self.report_failure(sandbox_id, original_dst, attempted_host, "tcp", reason)
    }

    /// A flow that connected and then lost its byte stream did not fail to
    /// connect, so it stays out of the connect-failure error. Which peer
    /// failed decides whether the destination had any part in it.
    pub(crate) fn report_tcp_transfer_failure(
        &self,
        sandbox_id: &str,
        original_dst: SocketAddr,
        attempted_host: &DomainName,
        failed_peer: TransferPeer,
        fault: &str,
        reason: &str,
    ) -> bool {
        let Some(destination) = self.destination(attempted_host) else {
            return false;
        };
        let destination = destination.as_str();
        let original_dst_port = original_dst.port();
        match failed_peer {
            // Clients routinely reset a connection they are done with; the
            // destination had already served it.
            TransferPeer::Guest => debug!(
                sandbox_id,
                original_dst_port,
                destination,
                protocol = "tcp",
                fault,
                reason,
                "guest abandoned an established known egress connection"
            ),
            TransferPeer::Destination => info!(
                sandbox_id,
                original_dst_port,
                destination,
                protocol = "tcp",
                fault,
                reason,
                "known egress destination failed an established connection"
            ),
        }
        true
    }

    fn destination(&self, attempted_host: &DomainName) -> Option<&DomainName> {
        self.domains
            .iter()
            .find(|destination| *destination == attempted_host)
    }

    fn report_failure(
        &self,
        sandbox_id: &str,
        original_dst: SocketAddr,
        attempted_host: &DomainName,
        protocol: &str,
        reason: &str,
    ) -> bool {
        let Some(destination) = self.destination(attempted_host) else {
            return false;
        };
        error!(
            sandbox_id,
            original_dst_port = original_dst.port(),
            destination = destination.as_str(),
            protocol,
            reason,
            "failed to connect to known egress destination"
        );
        true
    }

    pub(crate) fn report_dns_failure(
        &self,
        sandbox_id: &str,
        original_dst: SocketAddr,
        packet: &[u8],
        reason: &str,
    ) -> bool {
        if original_dst.port() != 53 {
            return false;
        }
        let Some(destination) = dns_host(packet) else {
            return false;
        };
        self.report_failure(sandbox_id, original_dst, &destination, "dns", reason)
    }
}

fn dns_host(packet: &[u8]) -> Option<DomainName> {
    let message = Message::from_vec(packet).ok()?;
    message.queries.first()?.name().to_string().parse().ok()
}

#[cfg(test)]
mod tests;
