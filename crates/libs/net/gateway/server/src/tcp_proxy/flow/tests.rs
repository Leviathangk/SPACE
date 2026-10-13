use super::*;
use crate::tcp_proxy::tls::HttpProtocol;

#[test]
fn cancellation_snapshots_the_live_http_request_phase() {
    for (active, phase) in [(0, Phase::HttpIdle), (8, Phase::HttpExchange)] {
        let flow = Flow::default();
        flow.at_phase(Phase::HttpIdle);
        let _requests: Vec<_> = (0..active)
            .map(|_| flow.request(HttpProtocol::Http2))
            .collect();
        flow.cancelled();
        let failure = flow.failure().unwrap();
        assert_eq!(failure.requests.in_flight, active);
        assert_eq!(failure.phase, phase);
        assert_eq!(failure.codes.close().as_str(), "local_reset");
    }
}
