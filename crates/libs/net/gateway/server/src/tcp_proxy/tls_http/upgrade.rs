//! Explicit transitions out of HTTP framing. WebSockets stay bound to their
//! origin. CONNECT is handled separately as an opaque byte relay.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use http::{HeaderValue, Method, Response, StatusCode, Version, header};
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use tokio_util::sync::CancellationToken;

use super::super::accounting::{Attempt, Outcome};
use super::super::leg::{RelayFault, RelayWitness};
use super::SanitizedRequest;

pub(super) enum Handshake {
    WebSocket { accept: HeaderValue },
}

impl Handshake {
    pub(super) fn request<B>(request: &SanitizedRequest<B>) -> Result<Option<Self>, ()> {
        if (request.method() == Method::CONNECT || request.headers().contains_key(header::UPGRADE))
            && (request.headers().contains_key(header::TRANSFER_ENCODING)
                || request
                    .headers()
                    .get(header::CONTENT_LENGTH)
                    .is_some_and(|length| length != "0"))
        {
            return Err(());
        }
        if request.method() == Method::CONNECT {
            return Err(());
        }
        if !request.headers().contains_key(header::UPGRADE) {
            return Ok(None);
        }
        if request.version() != Version::HTTP_11
            || request.method() != Method::GET
            || !websocket_headers(
                request.headers().get_all(header::UPGRADE),
                request.headers().get_all(header::CONNECTION),
            )
            || single(request.headers().get_all(header::SEC_WEBSOCKET_VERSION))? != "13"
        {
            return Err(());
        }
        let key = single(request.headers().get_all(header::SEC_WEBSOCKET_KEY))?;
        if STANDARD.decode(key.as_bytes()).map_err(|_| ())?.len() != 16 {
            return Err(());
        }
        // RFC 6455 fixes SHA-1 here; it is a handshake nonce, not certificate trust.
        let mut digest =
            aws_lc_rs::digest::Context::new(&aws_lc_rs::digest::SHA1_FOR_LEGACY_USE_ONLY);
        digest.update(key.as_bytes());
        digest.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        let accept = STANDARD.encode(digest.finish()).parse().map_err(|_| ())?;
        Ok(Some(Self::WebSocket { accept }))
    }

    pub(super) fn accepts<B>(&self, response: &Response<B>) -> bool {
        match self {
            Self::WebSocket { accept } => {
                response.status() == StatusCode::SWITCHING_PROTOCOLS
                    && websocket_headers(
                        response.headers().get_all(header::UPGRADE),
                        response.headers().get_all(header::CONNECTION),
                    )
                    && single(response.headers().get_all(header::SEC_WEBSOCKET_ACCEPT))
                        .is_ok_and(|value| value == accept)
            }
        }
    }

    pub(super) const fn as_str(&self) -> &'static str {
        match self {
            Self::WebSocket { .. } => "websocket",
        }
    }
}

fn single(values: header::GetAll<'_, HeaderValue>) -> Result<&HeaderValue, ()> {
    let mut values = values.into_iter();
    let value = values.next().ok_or(())?;
    if values.next().is_some() {
        return Err(());
    }
    Ok(value)
}

fn websocket_headers(
    upgrade: header::GetAll<'_, HeaderValue>,
    connection: header::GetAll<'_, HeaderValue>,
) -> bool {
    single(upgrade).is_ok_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"))
        && connection.iter().any(|value| {
            value.to_str().is_ok_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
        })
}

pub(super) async fn relay(
    guest: OnUpgrade,
    origin: OnUpgrade,
    shutdown: CancellationToken,
    protocol: &'static str,
) {
    let mut forwarding = Attempt::forwarding(protocol, "relay");
    let witness = RelayWitness::default();
    let started_at = tokio::time::Instant::now();
    let operation = async {
        let (guest, origin) = tokio::time::timeout(Duration::from_secs(10), async {
            tokio::try_join!(guest, origin)
        })
        .await
        .map_err(|_| "upgrade_timeout")?
        .map_err(|_| "upgrade_failed")?;
        let mut guest = witness.guest(TokioIo::new(guest));
        let mut origin = witness.origin(TokioIo::new(origin));
        // Polling bounds inactivity to 60–70 seconds without unbounded buffers
        // or per-byte notifications. The enclosing lease caps total lifetime.
        let idle = async {
            let mut bytes = 0;
            let mut last_progress = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let (guest_bytes, origin_bytes) = witness.bytes();
                let current = guest_bytes + origin_bytes;
                if current != bytes {
                    last_progress = tokio::time::Instant::now();
                    bytes = current;
                }
                if last_progress.elapsed() >= Duration::from_secs(60) {
                    return;
                }
            }
        };
        tokio::select! {
            () = idle => Err("upgrade_idle_timeout"),
            result = tokio::io::copy_bidirectional(&mut guest, &mut origin) => result
                .map(|_| ())
                // Name the peer that failed instead of one opaque label.
                .map_err(|_| witness.fault().map_or("upgrade_io_failed", RelayFault::as_str)),
        }
    };
    let outcome = tokio::select! {
        biased;
        () = shutdown.cancelled() => {
            forwarding.finish(Outcome::Cancelled, "session_ended");
            "upgrade_cancelled"
        },
        result = operation => match result {
            Ok(()) => {
                forwarding.finish(Outcome::Success, "websocket_complete");
                "upgrade_complete"
            }
            Err("upgrade_idle_timeout") => {
                forwarding.finish(Outcome::Success, "idle_timeout");
                "upgrade_idle_timeout"
            }
            Err(outcome) => {
                forwarding.finish(Outcome::Error, outcome);
                outcome
            }
        },
    };
    let (guest_bytes, origin_bytes) = witness.bytes();
    pplx_space_metrics::gateway::domain_binding("tcp", "tls_http", outcome, None);
    pplx_space_metrics::gateway::relay_duration_ms(
        protocol,
        outcome,
        witness.closed_by(),
        started_at.elapsed().as_secs_f64() * 1_000.0,
    );
    tracing::info!(
        boundary = "forwarding",
        protocol,
        operation = "relay",
        outcome,
        closed_by = witness.closed_by(),
        fault = witness.fault().map(RelayFault::as_str),
        guest_bytes,
        origin_bytes,
        "inspected HTTP upgrade ended"
    );
}
