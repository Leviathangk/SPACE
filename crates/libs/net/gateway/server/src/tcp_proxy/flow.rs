//! Observations owned by one connection and its inspected requests.

use super::tls::{HttpProtocol, TlsTransportError};
use parking_lot::Mutex;
use std::sync::Arc;

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
impl From<super::leg::Side> for Side {
    fn from(side: super::leg::Side) -> Self {
        match side {
            super::leg::Side::Guest => Self::Downstream,
            super::leg::Side::Origin => Self::Upstream,
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
    pub(super) aborted: u64,
}
impl Requests {
    fn phase(self) -> Phase {
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
    closed: Option<(Side, Requests)>,
    phase: Option<Phase>,
}
impl State {
    fn phase(&self) -> Option<Phase> {
        self.phase.map(|phase| match phase {
            Phase::HttpExchange | Phase::HttpIdle => self.requests.phase(),
            phase => phase,
        })
    }
}

#[derive(Clone)]
pub(super) struct Flow {
    state: Arc<Mutex<State>>,
    pub(super) id: uuid::Uuid,
    span: tracing::Span,
}
impl Default for Flow {
    fn default() -> Self {
        Self {
            state: Arc::default(),
            id: uuid::Uuid::now_v7(),
            span: tracing::Span::current(),
        }
    }
}
impl Flow {
    pub(super) fn at_phase(&self, phase: Phase) {
        self.state.lock().phase = Some(phase);
    }
    pub(super) fn requests(&self) -> Requests {
        self.state.lock().requests
    }
    pub(super) fn failure(&self) -> Option<Failure> {
        self.state.lock().failure
    }

    pub(super) fn failed(&self, error: &TlsTransportError, phase: Option<Phase>, side: Side) {
        self.observe(phase, side, error.as_str(), Codes::transport(error));
    }
    pub(super) fn io_failed(&self, error: &std::io::Error, phase: Option<Phase>, side: Side) {
        let outcome = match phase {
            Some(Phase::TcpConnect) => "connect",
            Some(Phase::Relay) => "transfer",
            _ => match side {
                Side::Downstream => "tls_downstream_error",
                Side::Upstream => "tls_upstream_error",
            },
        };
        self.observe(phase, side, outcome, Codes::io(error));
    }
    fn observe(&self, phase: Option<Phase>, side: Side, outcome: &'static str, codes: Codes) {
        let mut state = self.state.lock();
        let Some(phase) = phase.or_else(|| state.phase()) else {
            return;
        };
        // Hyper can drop request owners before surfacing an EOF-related error.
        // Retain the counts observed at that transport boundary.
        let requests = state
            .closed
            .filter(|(closed_side, _)| *closed_side == side)
            .map_or(state.requests, |(_, requests)| requests);
        let phase = match phase {
            Phase::HttpIdle | Phase::HttpExchange => requests.phase(),
            phase => phase,
        };
        let observed = Failure {
            phase,
            side,
            requests,
            outcome,
            codes,
        };
        match &mut state.failure {
            // A native TLS alert takes precedence over the socket error that follows it.
            Some(first)
                if first.side == side
                    && first.codes.tls_alert.is_none()
                    && codes.tls_alert.is_some() =>
            {
                first.codes = codes
            }
            None => state.failure = Some(observed),
            _ => {}
        }
    }
    pub(super) fn cancelled(&self) {
        self.failed(&TlsTransportError::Withdrawn, None, Side::Downstream);
    }
    pub(super) fn closed(&self, side: Side) {
        let mut state = self.state.lock();
        let requests = state.requests;
        state.closed.get_or_insert((side, requests));
    }
    pub(super) fn ending(&self) -> (Option<Phase>, Option<Side>) {
        let state = self.state.lock();
        (state.phase(), state.closed.map(|(side, _)| side))
    }
    pub(super) fn request(&self, protocol: HttpProtocol) -> Request {
        self.state.lock().requests.in_flight += 1;
        Request::new(self.clone(), protocol)
    }
}
