use std::sync::Arc;

use http::StatusCode;
use parking_lot::Mutex;

use super::codes::{Close, Codes};
use super::{Flow, Side};
use crate::tcp_proxy::tls::{HttpProtocol, TlsTransportError};

#[derive(Clone, Copy, Debug)]
pub(in crate::tcp_proxy) enum StatusSource {
    Gateway,
    Upstream,
}

impl StatusSource {
    pub(in crate::tcp_proxy) const fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Upstream => "upstream",
        }
    }
}

#[derive(Clone, Default)]
pub(in crate::tcp_proxy) struct RequestErrors(Arc<Mutex<Option<(Side, Codes)>>>);

impl RequestErrors {
    pub(in crate::tcp_proxy) fn observe(
        &self,
        error: &(dyn std::error::Error + 'static),
        side: Side,
    ) {
        self.0.lock().get_or_insert((side, Codes::error(error)));
    }
}

/// The service owns this until headers, then transfers it into the response
/// body. Dropping either owner ends precisely one request, including h2 streams
/// reset while the rest of the connection continues serving requests.
pub(in crate::tcp_proxy) struct Request {
    flow: Flow,
    protocol: HttpProtocol,
    response: Option<(StatusCode, StatusSource)>,
    injected: bool,
    errors: RequestErrors,
    finished: bool,
}

impl Request {
    pub(super) fn new(flow: Flow, protocol: HttpProtocol) -> Self {
        pplx_space_metrics::gateway::http_request_started(protocol.as_str());
        Self {
            flow,
            protocol,
            response: None,
            injected: false,
            errors: RequestErrors::default(),
            finished: false,
        }
    }

    pub(in crate::tcp_proxy) fn errors(&self) -> RequestErrors {
        self.errors.clone()
    }

    pub(in crate::tcp_proxy) fn failed(&self, error: &TlsTransportError, side: Side) {
        self.errors
            .0
            .lock()
            .get_or_insert((side, Codes::transport(error)));
    }

    pub(in crate::tcp_proxy) fn injected(&mut self, injected: bool) {
        self.injected = injected;
    }

    pub(in crate::tcp_proxy) fn response(&mut self, status: StatusCode, source: StatusSource) {
        self.response = Some((status, source));
    }

    pub(in crate::tcp_proxy) fn complete(&mut self) {
        self.finish(true);
    }

    fn finish(&mut self, completed: bool) {
        if self.finished {
            return;
        }
        self.finished = true;
        let mut state = self.flow.state.lock();
        state.requests.in_flight -= 1;
        if completed {
            state.requests.completed += 1;
        }
        let failure = self.errors.0.lock().or_else(|| {
            if completed {
                None
            } else {
                state.failure.map(|f| (f.side, f.codes))
            }
        });
        drop(state);
        let codes = failure.map(|(_, codes)| codes);
        let h2 = codes.and_then(Codes::h2_error_code);
        let errno = codes
            .and_then(|codes| codes.errno)
            .map(|errno| format!("{errno:?}"));
        let error_type = codes.map(Codes::error_type);
        let side = failure
            .map(|(side, _)| side.as_str())
            .or((!completed).then_some("downstream"));
        let close = codes
            .map(Codes::close)
            .unwrap_or(if completed {
                Close::Normal
            } else {
                Close::RemoteReset
            })
            .as_str();
        let outcome = if completed { "completed" } else { "aborted" };
        let status = self.response.map(|(status, _)| status.as_u16());
        let status_source = self.response.map(|(_, source)| source.as_str());
        let termination = pplx_space_metrics::gateway::Termination {
            side,
            close: Some(close),
            h2_error_code: h2.as_deref(),
            h2_origin: codes
                .and_then(|codes| codes.http)
                .and_then(|http| http.h2_origin),
            errno: errno.as_deref(),
            http_lib_reason: codes.and_then(Codes::http_lib_reason),
            error_type: error_type.as_deref().or((!completed).then_some("_OTHER")),
            ..Default::default()
        };
        pplx_space_metrics::gateway::http_request(
            self.protocol.as_str(),
            outcome,
            status,
            status_source,
            self.injected,
            &termination,
        );
        tracing::info!(
            connection_id = %self.flow.id,
            protocol = self.protocol.as_str(), outcome, side, close,
            http.response.status_code = status, status_source, injected = self.injected,
            h2_error_code = h2, h2_origin = termination.h2_origin,
            errno, http_lib_reason = termination.http_lib_reason, error.type = termination.error_type,
            "inspected HTTP request ended"
        );
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        self.finish(false);
    }
}
