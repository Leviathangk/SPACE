//! Facts from typed operation results. Opaque sources are never decoded.
//!
//! HTTP driver termination does not expose per-stream completion, occupancy or
//! native reset codes. Tokio-Rustls does not expose alert codes through its I/O
//! errors. Leave those fields absent rather than deriving them from error text,
//! source-chain downcasts, body production or task cancellation.
use super::{
    FlowOutcome, Rejection, RelayFailure, TcpProxy, http_host, http_session, interception_detail,
    tls::TlsTransportError, transport, upstream,
};
use nix::errno::Errno;
use pplx_space_metrics::gateway::{self, TcpConnectionLabels};
use pplx_space_net_common::{DenyReason, DomainName};
use std::{io, net::SocketAddr};

#[derive(Default)]
struct Facts {
    phase: Option<&'static str>,
    side: Option<&'static str>,
    close: Option<&'static str>,
    errno: Option<Errno>,
    io_kind: Option<io::ErrorKind>,
    http_lib_reason: Option<&'static str>,
    error_type: Option<&'static str>,
    guardrail_id: Option<&'static str>,
}

impl Facts {
    fn io(phase: &'static str, side: Option<&'static str>, error: &io::Error) -> Self {
        Self {
            phase: Some(phase),
            side,
            // An I/O operation failing is not proof of a locally generated RST.
            close: (side.is_some() && error.raw_os_error() == Some(libc::ECONNRESET))
                .then_some("remote_reset"),
            errno: error
                .raw_os_error()
                .map(Errno::from_raw)
                .filter(|errno| *errno != Errno::UnknownErrno),
            io_kind: Some(error.kind()),
            error_type: Some(if error.kind() == io::ErrorKind::TimedOut {
                "timeout"
            } else {
                "_OTHER"
            }),
            ..Self::default()
        }
    }

    fn connect(error: &transport::Error) -> Self {
        match error {
            transport::Error::Io(error) => Self::io("tcp_connect", Some("upstream"), error),
            transport::Error::Timeout => Self {
                phase: Some("tcp_connect"),
                side: Some("upstream"),
                error_type: Some("timeout"),
                ..Self::default()
            },
        }
    }

    fn tls(error: &TlsTransportError, side: &'static str) -> Self {
        match error {
            TlsTransportError::Connect(error) => Self::connect(error),
            TlsTransportError::Downstream(error) => {
                Self::io("tls_handshake", Some("downstream"), error)
            }
            TlsTransportError::Upstream(error) => {
                Self::io("tls_handshake", Some("upstream"), error)
            }
            TlsTransportError::Invalidated | TlsTransportError::Withdrawn => Self::default(),
            _ => Self {
                phase: Some("tls_handshake"),
                side: Some(side),
                error_type: Some(if matches!(error, TlsTransportError::Timeout) {
                    "timeout"
                } else {
                    "_OTHER"
                }),
                ..Self::default()
            },
        }
    }

    fn http(error: &hyper::Error) -> Self {
        let reason = http_session::http_reason(error);
        Self {
            http_lib_reason: Some(reason),
            error_type: Some(if reason == "other" { "_OTHER" } else { reason }),
            ..Self::default()
        }
    }

    fn setup(error: &upstream::Error) -> Self {
        match error {
            upstream::Error::Connect(error) => Self::connect(error),
            upstream::Error::Tls(error) => Self::tls(error, "upstream"),
            upstream::Error::Http(error) => Self {
                side: Some("upstream"),
                ..Self::http(error)
            },
            upstream::Error::Cancelled => Self::default(),
        }
    }

    fn flow(outcome: &FlowOutcome) -> Self {
        match outcome {
            FlowOutcome::RelayFailed(RelayFailure::Connect(error)) => Self::connect(error),
            FlowOutcome::RelayFailed(RelayFailure::Transfer { error, fault }) => {
                Self::io("relay", fault.map(|fault| fault.peer()), error)
            }
            FlowOutcome::RelayFailed(RelayFailure::Plaintext(http_host::Failure::Setup(error))) => {
                Self::setup(error)
            }
            FlowOutcome::RelayFailed(RelayFailure::Plaintext(http_host::Failure::GuestHttp(
                error,
            ))) => Self {
                side: Some("downstream"),
                ..Self::http(error)
            },
            FlowOutcome::RelayFailed(RelayFailure::Inspected(
                http_session::HttpError::Transport(error),
            )) => Self::tls(error, "downstream"),
            FlowOutcome::RelayFailed(RelayFailure::Inspected(
                http_session::HttpError::Protocol(error),
            )) => Self::http(error),
            FlowOutcome::RelayFailed(RelayFailure::Inspected(http_session::HttpError::Server(
                _,
            ))) => Self {
                error_type: Some("_OTHER"),
                ..Self::default()
            },
            FlowOutcome::Rejected(reason) => Self {
                side: Some("downstream"),
                guardrail_id: match reason {
                    Rejection::Admission(super::TcpAdmissionDenial::DnsAttribution(
                        DenyReason::PlatformGuardrail { guardrail_id },
                    )) => Some(guardrail_id),
                    _ => None,
                },
                ..Self::default()
            },
            _ => Self::default(),
        }
    }
}

pub(super) fn record(
    proxy: &TcpProxy,
    destination: SocketAddr,
    host: Option<&DomainName>,
    outcome: &FlowOutcome,
) {
    let (status, detail) = outcome.metric_tags();
    let facts = Facts::flow(outcome);
    let errno = facts.errno.map(|errno| format!("{errno:?}"));
    let labels = TcpConnectionLabels {
        phase: facts.phase,
        side: facts.side,
        close: facts.close,
        errno: errno.as_deref(),
        http_lib_reason: facts.http_lib_reason,
        // A native errno takes precedence over a library category or fallback.
        error_type: errno.as_deref().or(facts.error_type),
        guardrail_id: facts.guardrail_id,
    };
    // Preserve exactly one unsampled connection measurement, including old labels.
    gateway::tcp_connection(status, detail, &labels);
    if !matches!(
        outcome,
        FlowOutcome::Cancelled | FlowOutcome::Rejected(_) | FlowOutcome::RelayFailed(_)
    ) {
        return;
    }

    let connection_id = uuid::Uuid::now_v7();
    let sampled_relay = matches!(
        outcome,
        FlowOutcome::RelayFailed(RelayFailure::Transfer { fault: Some(_), .. })
    );
    // UUIDv7's random tail selects relay failures independently of error kind.
    if sampled_relay && !(connection_id.as_u128() as u64).is_multiple_of(100) {
        return;
    }
    let (emit, suppressed) = match outcome {
        FlowOutcome::Rejected(Rejection::TlsInterception(error)) => proxy
            .refusal_warnings
            .sample(error.as_str(), interception_detail(error)),
        FlowOutcome::Rejected(_) => proxy.refusal_warnings.sample(detail, facts.guardrail_id),
        _ => (true, 0),
    };
    if !emit {
        return;
    }
    let reason = match outcome {
        FlowOutcome::Rejected(Rejection::TlsInterception(error)) => Some(error.as_str()),
        _ => None,
    };
    let rejection_detail = match outcome {
        FlowOutcome::Rejected(Rejection::TlsInterception(error)) => interception_detail(error),
        _ => None,
    };
    let inspected_host = matches!(
        outcome,
        FlowOutcome::RelayFailed(RelayFailure::Inspected(_))
    )
    .then_some(host)
    .flatten();
    tracing::info!(
        telemetry_type = "gateway_tcp_connection",
        sandbox_id = proxy.sandbox_id,
        %connection_id,
        destination_ip = %destination.ip(),
        destination_port = destination.port(),
        tls.server_name = inspected_host.map(DomainName::as_str),
        status, detail, reason, rejection_detail,
        phase = labels.phase,
        side = labels.side,
        close = labels.close,
        errno = labels.errno,
        io_kind = ?facts.io_kind,
        http_lib_reason = labels.http_lib_reason,
        error.type = labels.error_type,
        guardrail_id = labels.guardrail_id,
        // Refusals use the per-cause budget, not probability sampling.
        sample_rate = sampled_relay.then_some(0.01_f64),
        suppressed,
        "TCP connection ended"
    );
}
