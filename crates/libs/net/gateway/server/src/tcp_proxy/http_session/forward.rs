//! One authorization, readiness and submission path for both HTTP entries.
use super::super::http_host::{
    BindingOutcome, normalize_upstream_routing, validated_request_authority,
};
use super::credentials::{credential_headers, inject};
use super::*;
use http::{HeaderMap, StatusCode, Version, header, uri::Authority};
use pplx_space_net_common::DomainName;

enum Kind {
    Request,
    WebSocket(upgrade::Handshake),
    Connect,
}

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
        let mut authorization = self
            .mode
            .is_inspected()
            .then(|| Attempt::http_authorization(protocol.as_str()));
        let authorized = async {
            let (host, authority, kind) = match &self.mode {
                Mode::Plaintext => {
                    let (host, authority) = validated_request_authority(&request)
                        .map_err(|()| BindingOutcome::InvalidHost)?;
                    if request.method() == http::Method::CONNECT
                        || request.headers().contains_key(header::UPGRADE)
                    {
                        return Err(RequestError::UnsupportedUpgrade);
                    }
                    (host, Some(authority), Kind::Request)
                }
                Mode::Inspected(lease) => {
                    if !lease.is_usable() {
                        return Err(RequestError::Cancelled);
                    }
                    if request.method() == http::Method::CONNECT {
                        let target = connect::target(&request)
                            .map_err(|()| RequestError::InvalidConnect)?
                            .to_string();
                        *request.uri_mut() = target.parse().expect("validated CONNECT target");
                        request.set_host(target.parse().expect("validated CONNECT target"));
                        (lease.host().clone(), None, Kind::Connect)
                    } else {
                        authorize_authority(&request, lease.host(), lease.port())?;
                        let handshake = upgrade::Handshake::request(&request)
                            .map_err(|()| RequestError::UnsupportedUpgrade)?;
                        (
                            lease.host().clone(),
                            None,
                            handshake.map_or(Kind::Request, Kind::WebSocket),
                        )
                    }
                }
            };
            let policy = self.binding.authorize(&host).await?;
            if let Mode::Inspected(lease) = &self.mode
                && !matches!(kind, Kind::Connect)
            {
                let headers = credential_headers(lease, &request)?;
                if matches!(kind, Kind::WebSocket(_)) && !headers.is_empty() {
                    return Err(RequestError::CredentialUpgrade);
                }
            }
            Ok((host, authority, kind, policy))
        }
        .await;
        if let Some(attempt) = &mut authorization {
            match &authorized {
                Ok(_) => attempt.finish(Outcome::Success, "authorized"),
                Err(error) => attempt.finish(error.outcome(), error.reason(true)),
            }
        }
        let (host, authority, kind, policy) = authorized?;
        let mut forwarding = self
            .mode
            .is_inspected()
            .then(|| Attempt::forwarding(protocol.as_str(), "request"));
        let result = async {
            let sender = self.upstream.sender(protocol).await?;
            let sender = sender.ready().await?;
            if self.shutdown.is_cancelled() {
                return Err(RequestError::Cancelled);
            }
            // No request headers are submitted before the final authorization check.
            let injected = match &self.mode {
                Mode::Plaintext => {
                    self.binding.reauthorize(&host, &policy).await?;
                    false
                }
                Mode::Inspected(lease) => {
                    if !lease.is_usable() {
                        return Err(RequestError::Cancelled);
                    }
                    if !matches!(kind, Kind::Connect) {
                        let headers = credential_headers(lease, &request)?;
                        let injected = !headers.is_empty();
                        inject(&mut request, headers);
                        injected
                    } else {
                        false
                    }
                }
            };
            if self.shutdown.is_cancelled() {
                return Err(RequestError::Cancelled);
            }
            if matches!(kind, Kind::Connect) && !sender.is_http1() {
                return Err(RequestError::InvalidConnect);
            }
            if matches!(kind, Kind::WebSocket(_)) {
                request.set_websocket_upgrade();
            }
            let guest_upgrade = (!matches!(kind, Kind::Request)).then(|| request.on_upgrade());
            if let Some(authority) = &authority {
                normalize_upstream_routing(&mut request, authority)
                    .map_err(|()| BindingOutcome::InvalidHost)?;
            } else if sender.is_http1() && !matches!(kind, Kind::Connect) {
                translate_to_http1(&mut request);
            }
            let mut response =
                sender
                    .send(request.map(|body| {
                        Either::Left(self.forwarding_body(protocol, body, "request_body"))
                    }))
                    .await?;
            let upgrade = match &kind {
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
                self.record(
                    protocol,
                    outcome,
                    response.status(),
                    HttpResponseSource::Upstream { injected },
                );
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
            self.record(
                protocol,
                outcome,
                response.status(),
                HttpResponseSource::Upstream { injected },
            );
            Ok(response
                .map(|body| Either::Left(self.forwarding_body(protocol, body, "response_body"))))
        }
        .await;
        if let Some(attempt) = &mut forwarding {
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
}

fn translate_to_http1<B>(request: &mut SanitizedRequest<B>) {
    // Preserve the exact wire authority when translating: default ports can
    // change signatures.
    if !request.headers().contains_key(header::HOST) {
        let authority = request
            .uri()
            .authority()
            .expect("validated HTTP/2 authority");
        let host = authority.as_str().parse().expect("validated authority");
        request.set_host(host);
    }
    *request.uri_mut() = request
        .uri()
        .path_and_query()
        .map_or("/", |path| path.as_str())
        .parse()
        .expect("Hyper parsed path");
    *request.version_mut() = Version::HTTP_11;
}

fn websocket_response_headers(headers: &mut HeaderMap) {
    headers.insert(
        header::CONNECTION,
        http::HeaderValue::from_static("upgrade"),
    );
    headers.insert(header::UPGRADE, http::HeaderValue::from_static("websocket"));
}

#[derive(Debug, thiserror::Error)]
pub(super) enum AuthorityError {
    #[error("HTTP request authority is missing or malformed")]
    Invalid,
    #[error("HTTP request authority differs from the admitted TLS origin")]
    Mismatch,
}

impl AuthorityError {
    pub(super) const fn as_str(&self) -> &'static str {
        match self {
            Self::Invalid => "invalid_authority",
            Self::Mismatch => "authority_mismatch",
        }
    }
}

pub(super) fn authorize_authority<B>(
    request: &SanitizedRequest<B>,
    host: &DomainName,
    port: u16,
) -> Result<(), AuthorityError> {
    let validate = |authority: &Authority| {
        if authority.as_str().contains('@')
            || authority.as_str().ends_with(':')
            || (authority.as_str().contains(':') && authority.port_u16().is_none())
        {
            return Err(AuthorityError::Invalid);
        }
        let name: DomainName = authority
            .host()
            .parse()
            .map_err(|_| AuthorityError::Invalid)?;
        if &name != host || authority.port_u16().unwrap_or(443) != port {
            return Err(AuthorityError::Mismatch);
        }
        Ok(())
    };
    if request
        .uri()
        .scheme_str()
        .is_some_and(|scheme| scheme != "https")
    {
        return Err(AuthorityError::Mismatch);
    }
    if let Some(authority) = request.uri().authority() {
        validate(authority)?;
    } else if request.version() == Version::HTTP_2 {
        return Err(AuthorityError::Invalid);
    }
    let mut hosts = request.headers().get_all(header::HOST).iter();
    if let Some(value) = hosts.next() {
        if hosts.next().is_some() {
            return Err(AuthorityError::Invalid);
        }
        let authority = value
            .to_str()
            .ok()
            .and_then(|value| value.parse::<Authority>().ok())
            .ok_or(AuthorityError::Invalid)?;
        validate(&authority)?;
    } else if request.version() != Version::HTTP_2 {
        return Err(AuthorityError::Invalid);
    }
    Ok(())
}
