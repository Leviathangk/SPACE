//! One authorization, readiness and submission path for both HTTP entries.
//!
//! The order of checks is the security property: policy is consulted before
//! origin setup, and checked again after readiness, before any request header
//! reaches the origin. Mode differences live behind [`Mode`].
use std::sync::Arc;

use http::{HeaderMap, Response, StatusCode, header};
use http_body_util::{Either, Empty};
use hyper::body::Incoming;
use pplx_space_net_common::NetworkPolicyV1;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::Instrument;

use super::super::accounting::{Attempt, Outcome};
use super::super::http_protocol::HttpProtocol;
use super::super::routing_headers::SanitizedRequest;
use super::super::upstream;
use super::error::RequestError;
use super::mode::{Kind, Target};
use super::{Body, Session, upgrade};

impl<F, Fut, U> Session<F>
where
    F: FnOnce(HttpProtocol) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(U, HttpProtocol), upstream::Error>> + Send + 'static,
    U: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pub(super) async fn forward(
        &self,
        mut request: SanitizedRequest<Incoming>,
    ) -> Result<Response<Body>, RequestError> {
        let protocol = HttpProtocol::of(request.version());
        let (target, policy) = self.authorize(protocol, &mut request).await?;
        let mut attempt = self
            .mode
            .attempt(|| Attempt::forwarding(protocol.as_str(), "request"));
        let result = self.submit(protocol, request, target, policy).await;
        if let Some(attempt) = &mut attempt {
            match &result {
                Ok(_) => attempt.finish(Outcome::Success, "response_headers"),
                Err(RequestError::Setup(_)) => attempt.finish(Outcome::Error, "origin_setup"),
                Err(RequestError::Upstream(_)) => attempt.finish(Outcome::Error, "upstream_http"),
                Err(RequestError::Credential(error)) => {
                    attempt.finish(Outcome::Cancelled, error.as_str());
                }
                Err(error) => attempt.finish(error.outcome(), error.reason(true)),
            }
        }
        result
    }

    /// Steps 1-3, before any origin connection exists.
    async fn authorize(
        &self,
        protocol: HttpProtocol,
        request: &mut SanitizedRequest<Incoming>,
    ) -> Result<(Target, Arc<NetworkPolicyV1>), RequestError> {
        let mut attempt = self
            .mode
            .attempt(|| Attempt::http_authorization(protocol.as_str()));
        let authorized = async {
            // 1. What the request targets, and forms this mode refuses.
            let target = self.mode.target(request)?;
            // 2. Whether policy lets the sandbox reach that host here.
            let policy = self.binding.authorize(&target.host).await?;
            // 3. Mode checks that need a policy allow (inspected credentials).
            self.mode.admit(request, &target)?;
            Ok::<_, RequestError>((target, policy))
        }
        .await;
        if let Some(attempt) = &mut attempt {
            match &authorized {
                Ok(_) => attempt.finish(Outcome::Success, "authorized"),
                Err(error) => attempt.finish(error.outcome(), error.reason(true)),
            }
        }
        authorized
    }

    /// Steps 4-6: origin readiness, final re-check, submission and response.
    async fn submit(
        &self,
        protocol: HttpProtocol,
        mut request: SanitizedRequest<Incoming>,
        target: Target,
        policy: Arc<NetworkPolicyV1>,
    ) -> Result<Response<Body>, RequestError> {
        // 4. Origin readiness. Setup is owned by the connection, not this request.
        let sender = self.upstream.sender(protocol).await?;
        let sender = sender.ready().await?;
        if self.shutdown.is_cancelled() {
            return Err(RequestError::Cancelled);
        }
        // 5. Final check. No request headers are submitted before it passes.
        self.mode
            .recheck(&self.binding, &target, &policy, &mut request)
            .await?;
        if self.shutdown.is_cancelled() {
            return Err(RequestError::Cancelled);
        }
        // 6. Submit.
        let kind = &target.kind;
        if matches!(kind, Kind::Connect) && !sender.is_http1() {
            return Err(RequestError::InvalidConnect);
        }
        if matches!(kind, Kind::WebSocket(_)) {
            request.set_websocket_upgrade();
        }
        let guest_upgrade = (!matches!(kind, Kind::Request)).then(|| request.on_upgrade());
        self.mode.route(&target, &mut request, sender.is_http1())?;
        let mut response = sender
            .send(
                request
                    .map(|body| Either::Left(self.forwarding_body(protocol, body, "request_body"))),
            )
            .await?;
        let upgrade = match kind {
            Kind::Connect if response.status().is_success() => {
                Some((upgrade::Kind::Connect, "connect_forwarded"))
            }
            Kind::WebSocket(handshake) if handshake.accepts(&response) => {
                Some((upgrade::Kind::WebSocket, handshake.as_str()))
            }
            _ => None,
        };
        if let Some((kind, outcome)) = upgrade {
            let origin = hyper::upgrade::on(&mut response);
            let guest = guest_upgrade.expect("only upgrade requests reach handoff");
            response.headers_mut().remove(header::CONTENT_LENGTH);
            if matches!(kind, upgrade::Kind::WebSocket) {
                websocket_response_headers(response.headers_mut());
            }
            self.upgrades.spawn(
                upgrade::relay(
                    guest,
                    origin,
                    self.shutdown.clone(),
                    protocol.as_str(),
                    kind,
                )
                .in_current_span(),
            );
            self.mode.record(protocol, outcome, response.status());
            return Ok(response.map(|_| Either::Right(Empty::new())));
        }
        if response.status() == StatusCode::SWITCHING_PROTOCOLS {
            return Err(RequestError::UnexpectedUpgrade);
        }
        let outcome = if matches!(kind, Kind::Connect) {
            "connect_proxy_refused"
        } else {
            "allowed"
        };
        self.mode.record(protocol, outcome, response.status());
        Ok(
            response
                .map(|body| Either::Left(self.forwarding_body(protocol, body, "response_body"))),
        )
    }
}

fn websocket_response_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CONNECTION,
        http::HeaderValue::from_static("upgrade"),
    );
    headers.insert(header::UPGRADE, http::HeaderValue::from_static("websocket"));
}
