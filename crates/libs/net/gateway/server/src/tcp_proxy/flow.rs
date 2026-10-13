//! Connection-local observations, shared with HTTP streams and origin drivers.

use std::sync::Arc;

use parking_lot::Mutex;

use super::tls::TlsTransportError;

pub(super) mod codes;
mod request;
#[cfg(test)]
mod tests;
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
    phase: Option<Phase>,
}

impl State {
    fn current_phase(&self) -> Option<Phase> {
        self.phase.map(|phase| match phase {
            Phase::HttpExchange | Phase::HttpIdle => self.requests.phase(),
            phase => phase,
        })
    }

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

#[derive(Clone)]
pub(super) struct Flow {
    state: Arc<Mutex<State>>,
    pub(super) id: uuid::Uuid,
}

impl Default for Flow {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            id: uuid::Uuid::now_v7(),
        }
    }
}

impl Flow {
    pub(super) fn at_phase(&self, phase: Phase) {
        self.state.lock().phase = Some(phase);
    }

    pub(super) fn cancelled(&self) {
        let mut state = self.state.lock();
        if let Some(phase) = state.current_phase() {
            let requests = state.requests;
            state.observe(Failure {
                phase,
                side: Side::Downstream,
                requests,
                outcome: "shutdown_or_policy_resweep",
                codes: Codes::default(),
            });
        }
    }

    pub(super) fn requests(&self) -> Requests {
        self.state.lock().requests
    }

    pub(super) fn failure(&self) -> Option<Failure> {
        self.state.lock().failure
    }

    pub(super) fn failed(&self, error: &TlsTransportError, phase: Phase, side: Side) {
        let mut state = self.state.lock();
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
        let mut state = self.state.lock();
        let requests = state.requests;
        let phase = state.current_phase().unwrap_or(requests.phase());
        state.observe(Failure {
            phase,
            side,
            requests,
            outcome: error.as_str(),
            codes: Codes::transport(error),
        });
    }

    pub(super) fn http_io_failed(&self, error: &std::io::Error, side: Side) {
        let mut state = self.state.lock();
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
        let mut state = self.state.lock();
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
        self.state.lock().closed.get_or_insert((phase, side));
    }

    pub(super) fn closed_by(&self) -> Option<(Phase, Side)> {
        self.state.lock().closed
    }

    pub(super) fn http_closed(&self, side: Side) {
        let mut state = self.state.lock();
        let phase = state.requests.phase();
        state.closed.get_or_insert((phase, side));
    }

    pub(super) fn request(&self, protocol: super::tls::HttpProtocol) -> Request {
        self.state.lock().requests.in_flight += 1;
        Request::new(self.clone(), protocol)
    }
}
