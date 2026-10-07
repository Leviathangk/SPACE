//! Operator-owned network policy additions, selected per federated create.

use axum::http::HeaderMap;
use pplx_space_net_common::{EgressDefaultAction, EgressDestinationV1, NetworkPolicyV1};

pub(crate) const OVERLAY_HEADER: &str = "x-space-network-policy-overlay";
const ENV: &str = "SPACE_NETWORK_POLICY_OVERLAY";

mod legacy;

pub(crate) fn network_policy_overlay_requested(headers: &HeaderMap, federated: bool) -> bool {
    let Some(value) = headers.get(OVERLAY_HEADER) else {
        return false;
    };
    if federated
        && value
            .to_str()
            .is_ok_and(|value| value.trim().eq_ignore_ascii_case("true"))
    {
        return true;
    }
    tracing::warn!("Network policy overlay ignored: invalid request");
    pplx_space_metrics::sandbox::admission::network_policy_overlay_create(
        pplx_space_metrics::sandbox::admission::NetworkPolicyOverlayOutcome::InvalidRequest,
    );
    false
}

pub(crate) fn ecw_network_policy_overlay_requested(headers: &HeaderMap, federated: bool) -> bool {
    // An explicit generic header takes precedence, including an opt-out.
    if headers.contains_key(OVERLAY_HEADER) {
        return network_policy_overlay_requested(headers, federated);
    }
    let Some(value) = headers.get(legacy::HEADER) else {
        return false;
    };
    if federated
        && value
            .to_str()
            .is_ok_and(|value| value.trim().eq_ignore_ascii_case("us2"))
    {
        return true;
    }
    tracing::warn!("Network policy overlay ignored: invalid compatibility request");
    pplx_space_metrics::sandbox::admission::network_policy_overlay_create(
        pplx_space_metrics::sandbox::admission::NetworkPolicyOverlayOutcome::InvalidRequest,
    );
    false
}

/// Additive permissions and TLS passthrough are deliberately configured separately.
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NetworkPolicyOverlay {
    #[serde(default)]
    egress_allowances: Vec<EgressDestinationV1>,
    #[serde(default)]
    tls_interception_exemptions: Vec<EgressDestinationV1>,
}

#[derive(Debug, thiserror::Error)]
pub enum NetworkPolicyOverlayConfigError {
    #[error("{ENV} is not valid Unicode")]
    Environment(#[source] std::env::VarError),
    #[error("invalid {ENV}: {0}")]
    Json(#[source] serde_json::Error),
    #[error(transparent)]
    Legacy(#[from] legacy::ConfigError),
}

impl NetworkPolicyOverlay {
    pub(crate) fn from_env() -> Result<Option<Self>, NetworkPolicyOverlayConfigError> {
        Self::from_environment(|name| std::env::var(name))
    }

    fn from_environment(
        mut read: impl FnMut(&str) -> Result<String, std::env::VarError>,
    ) -> Result<Option<Self>, NetworkPolicyOverlayConfigError> {
        match read(ENV) {
            Ok(raw) => Self::from_config(&raw).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(legacy::from_environment(&mut read)?),
            Err(error) => Err(NetworkPolicyOverlayConfigError::Environment(error)),
        }
    }

    pub(crate) fn from_config(raw: &str) -> Result<Self, NetworkPolicyOverlayConfigError> {
        // Deserialize a map first: serde's struct visitor also accepts JSON
        // arrays, which are not part of the operator configuration contract.
        let object: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(raw).map_err(NetworkPolicyOverlayConfigError::Json)?;
        serde_json::from_value(serde_json::Value::Object(object))
            .map_err(NetworkPolicyOverlayConfigError::Json)
    }

    /// Explicit and platform denies retain precedence over additive allowances.
    pub(crate) fn compose(&self, base: &NetworkPolicyV1) -> NetworkPolicyV1 {
        let mut policy = base.clone();
        for destination in &self.egress_allowances {
            if !policy.egress.allow.contains(destination) {
                policy.egress.allow.push(destination.clone());
            }
        }
        for destination in &self.tls_interception_exemptions {
            if !policy.tls_interception_exemptions.contains(destination) {
                policy.tls_interception_exemptions.push(destination.clone());
            }
        }
        // Hostname additions need reachable resolvers even when the base is a
        // template policy that never passed through ECW request translation.
        if !self.egress_allowances.is_empty()
            && policy.default_action() == EgressDefaultAction::Deny
        {
            policy.egress.allow = crate::e2b::with_ecw_dns_resolver_allows(policy.egress.allow);
        }
        policy
    }
}

/// An unavailable optional overlay leaves ordinary create admission intact.
pub(crate) fn for_create(
    requested: bool,
    configured: Option<&NetworkPolicyOverlay>,
) -> Option<&NetworkPolicyOverlay> {
    use pplx_space_metrics::sandbox::admission::{
        NetworkPolicyOverlayOutcome, network_policy_overlay_create,
    };

    if !requested {
        return None;
    }
    if configured.is_some() {
        network_policy_overlay_create(NetworkPolicyOverlayOutcome::Ready);
    } else {
        tracing::warn!("Network policy overlay ignored: no overlay configured");
        network_policy_overlay_create(NetworkPolicyOverlayOutcome::Unconfigured);
    }
    configured
}

#[cfg(test)]
mod tests;
