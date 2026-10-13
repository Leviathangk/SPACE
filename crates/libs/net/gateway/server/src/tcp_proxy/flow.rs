//! Connection-local observations, shared with HTTP streams and origin drivers.

use std::sync::Arc;

use parking_lot::Mutex;

use super::tls::TlsTransportError;

pub(super) mod codes;
mod request;
use codes::Codes;
pub(super) use request::{Request, RequestErrors, StatusSource};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Side {
    Downstream,
    Upstream,
}

impl Side {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Downstream => "downstream",
            Self::Upstream => "upstream",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Phase {
    TcpConnect,
    TlsHandshake,
    HttpExchange,
    HttpIdle,
    Relay,
}

impl Phase {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::TcpConnect => "tcp_connect",
            Self::TlsHandshake => "tls_handshake",
            Self::HttpExchange => "http_exchange",
            Self::HttpIdle => "http_idle",
            Self::Relay => "relay",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Requests {
    pub(super) in_flight: u64,
    pub(super) completed: u64,
}

impl Requests {
    pub(super) const fn active_rq(self) -> &'static str {
        if self.in_flight == 0 { "0" } else { "1+" }
    }

    const fn phase(self) -> Phase {
        if self.in_flight == 0 {
            Phase::HttpIdle
        } else {
            Phase::HttpExchange
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Failure {
    pub(super) phase: Phase,
    pub(super) side: Side,
    pub(super) requests: Requests,
    pub(super) outcome: &'static str,
    pub(super) codes: Codes,
}

#[derive(Default)]
struct State {
    requests: Requests,
    failure: Option<Failure>,
    closed: Option<(Phase, Side)>,
}

impl State {
    fn observe(&mut self, failure: Failure) {
        match &mut self.failure {
            Some(first)
                if first.side == failure.side
                    && !first.codes.has_protocol_code()
                    && failure.codes.has_protocol_code() =>
            {
                first.codes = failure.codes;
            }
            None => self.failure = Some(failure),
            _ => {}
        }
    }
}

#[derive(Clone, Default)]
pub(super) struct Flow(Arc<Mutex<State>>);

impl Flow {
    pub(super) fn failure(&self) -> Option<Failure> {
        self.0.lock().failure
    }

    pub(super) fn failed(&self, error: &TlsTransportError, phase: Phase, side: Side) {
        let mut state = self.0.lock();
        let requests = state.requests;
        state.observe(Failure {
            phase,
            side,
            requests,
            outcome: error.as_str(),
            codes: Codes::transport(error),
        });
    }

    pub(super) fn http_failed(&self, error: &TlsTransportError, side: Side) {
        let mut state = self.0.lock();
        let requests = state.requests;
        state.observe(Failure {
            phase: requests.phase(),
            side,
            requests,
            outcome: error.as_str(),
            codes: Codes::transport(error),
        });
    }

    pub(super) fn http_io_failed(&self, error: &std::io::Error, side: Side) {
        let mut state = self.0.lock();
        let requests = state.requests;
        state.observe(Failure {
            phase: requests.phase(),
            side,
            requests,
            outcome: match side {
                Side::Downstream => "tls_downstream_error",
                Side::Upstream => "tls_upstream_error",
            },
            codes: Codes::io(error),
        });
    }

    pub(super) fn io_failed(&self, error: &std::io::Error, phase: Phase, side: Side) {
        let mut state = self.0.lock();
        let requests = state.requests;
        state.observe(Failure {
            phase,
            side,
            requests,
            codes: Codes::io(error),
            outcome: if phase == Phase::TcpConnect {
                "connect"
            } else {
                "transfer"
            },
        });
    }

    pub(super) fn closed(&self, phase: Phase, side: Side) {
        self.0.lock().closed.get_or_insert((phase, side));
    }

    pub(super) fn closed_by(&self) -> Option<(Phase, Side)> {
        self.0.lock().closed
    }

    pub(super) fn request(&self, protocol: super::tls::HttpProtocol) -> Request {
        self.0.lock().requests.in_flight += 1;
        Request::new(self.clone(), protocol)
    }
}
