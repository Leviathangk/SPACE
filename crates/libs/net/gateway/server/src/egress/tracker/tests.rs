use super::*;
use crate::egress::AdmittedTcp;
use crate::tls_sni::ClientHelloScan;
use pplx_space_net_common::TlsInterception;

#[test]
fn tracker_unregisters_finished_connections() {
    let tracker = DnsAttributedTcpTracker::default();
    let token = CancellationToken::new();
    {
        let admission = TcpAdmissionEvidence::new(
            AdmittedTcp::Tunnel("192.0.2.10:443".parse().unwrap()),
            ClientHelloScan::NoServerName,
            TlsInterception::Off,
            None,
        );
        let _connection = tracker.register(admission, token.clone());
    }

    assert_eq!(tracker.cancel_all(), 0);
    assert!(!token.is_cancelled());
}
