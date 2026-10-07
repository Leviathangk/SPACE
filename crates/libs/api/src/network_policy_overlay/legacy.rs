//! Compatibility inputs for deployments that have not migrated to the generic overlay.

use super::NetworkPolicyOverlay;
use pplx_space_net_common::EgressDestinationV1;
use std::env::VarError;

pub(super) const HEADER: &str = "x-space-falcon";
const DESTINATIONS_ENV: &str = "SPACE_FALCON_US2_EGRESS_DESTINATIONS";
const EXEMPTIONS_ENV: &str = "SPACE_FALCON_TLS_INTERCEPTION_EXEMPTIONS_ENABLED";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{DESTINATIONS_ENV} is not valid Unicode")]
    Environment(#[source] VarError),
    #[error("{EXEMPTIONS_ENV} is not valid Unicode")]
    ExemptionsEnvironment(#[source] VarError),
    #[error("{EXEMPTIONS_ENV} must be true or false")]
    ExemptionsActivation(#[source] std::str::ParseBoolError),
    #[error("invalid {DESTINATIONS_ENV}: {0}")]
    Json(#[source] serde_json::Error),
    #[error(
        "{DESTINATIONS_ENV} must contain at least one destination; unset it to disable the overlay"
    )]
    Empty,
    #[error("{DESTINATIONS_ENV} accepts only exact IP and domain destinations")]
    UnsupportedDestination,
}

pub(super) fn from_environment(
    read: &mut impl FnMut(&str) -> Result<String, VarError>,
) -> Result<Option<NetworkPolicyOverlay>, ConfigError> {
    let activation = match read(EXEMPTIONS_ENV) {
        Ok(raw) => Some(raw),
        Err(VarError::NotPresent) => None,
        Err(error) => return Err(ConfigError::ExemptionsEnvironment(error)),
    };
    let raw = match read(DESTINATIONS_ENV) {
        Ok(raw) => raw,
        Err(VarError::NotPresent) => return Ok(None),
        Err(error) => return Err(ConfigError::Environment(error)),
    };
    let exemptions_enabled = activation
        .as_deref()
        .unwrap_or("false")
        .parse::<bool>()
        .map_err(ConfigError::ExemptionsActivation)?;
    let destinations: Vec<EgressDestinationV1> =
        serde_json::from_str(&raw).map_err(ConfigError::Json)?;
    if destinations.is_empty() {
        return Err(ConfigError::Empty);
    }
    if destinations.iter().any(|destination| {
        !matches!(
            destination,
            EgressDestinationV1::Ip(_) | EgressDestinationV1::Domain(_)
        )
    }) {
        return Err(ConfigError::UnsupportedDestination);
    }
    Ok(Some(NetworkPolicyOverlay {
        tls_interception_exemptions: if exemptions_enabled {
            destinations.clone()
        } else {
            Vec::new()
        },
        egress_allowances: destinations,
    }))
}
