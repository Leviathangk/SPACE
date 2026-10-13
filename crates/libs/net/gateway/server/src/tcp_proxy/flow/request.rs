use super::super::tls::{HttpProtocol, TlsTransportError};
use super::{
    Flow, Side,
    codes::{Close, Codes},
};
use http::StatusCode;
use parking_lot::Mutex;
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(in crate::tcp_proxy) enum StatusSource {
    Gateway,
    Upstream,
}
impl StatusSource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Upstream => "upstream",
        }
    }
}

#[derive(Clone, Default)]
pub(in crate::tcp_proxy) struct RequestErrors(Arc<Mutex<Option<(Side, Codes)>>>);
impl RequestErrors {
    pub(in crate::tcp_proxy) fn observe(&self, error: &hyper::Error, side: Side) {
        self.0.lock().get_or_insert((side, Codes::http(error)));
    }
}

/// The service transfers this owner to the response body. Every exit ends one
/// request, including a dropped HTTP/2 stream on an otherwise live connection.
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
    pub(in crate::tcp_proxy) fn abort(&mut self) {
        self.finish(false);
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
        } else {
            state.requests.aborted += 1;
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
        let errno = codes
            .and_then(|codes| codes.errno)
            .map(|errno| format!("{errno:?}"));
        let error_type = codes.map(Codes::error_type);
        let termination = pplx_space_metrics::gateway::Termination {
            side: failure.map(|(side, _)| side.as_str()),
            close: codes
                .and_then(|codes| codes.close)
                .or(completed.then_some(Close::Normal))
                .map(Close::as_str),
            tls_alert: codes.and_then(|codes| codes.tls_alert),
            tls_alert_dir: codes.and_then(|codes| codes.tls_alert_dir),
            tls_cert_error: codes.and_then(|codes| codes.tls_cert_error),
            errno: errno.as_deref(),
            http_lib_reason: codes.and_then(|codes| codes.http_lib_reason),
            error_type: error_type.as_deref().or((!completed).then_some("_OTHER")),
            ..Default::default()
        };
        let outcome = if completed { "completed" } else { "aborted" };
        let status = self.response.map(|(status, _)| status.as_u16());
        let source = self.response.map(|(_, source)| source.as_str());
        pplx_space_metrics::gateway::http_request(
            self.protocol.as_str(),
            outcome,
            status,
            source,
            self.injected,
            &termination,
        );
        tracing::info!(parent: &self.flow.span,
            connection_id = %self.flow.id, protocol = self.protocol.as_str(), outcome,
            side = termination.side, close = termination.close,
            http.response.status_code = status, status_source = source, injected = self.injected,
            tls_alert = termination.tls_alert, tls_alert_dir = termination.tls_alert_dir,
            tls_cert_error = termination.tls_cert_error, errno = termination.errno,
            http_lib_reason = termination.http_lib_reason, error.type = termination.error_type,
            "inspected HTTP request ended"
        );
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        self.abort();
    }
}
