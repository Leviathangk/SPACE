//! Builder-side use of node-manager's verified environment, separate from guest
//! process environment variables and from downloadable artifact catalog entries.
use std::path::Path;

use pplx_space_common_core::types::{EnvironmentReference, ExecutionMetadata, TemplateMetadata};
use pplx_space_execution_common::{
    ExecutionProtoError,
    environment::{EnvironmentBindingError, EnvironmentDecodeError, EnvironmentDefinition},
};
use pplx_space_node_mgr_client::proto::{
    ResolveExecutionEnvironmentRequest, ResolveExecutionEnvironmentResponse,
};

#[derive(Debug, thiserror::Error)]
pub enum BuildExecutionBindingError {
    #[error("cannot connect to environment resolver: {0}")]
    Connect(#[from] pplx_space_node_mgr_client::ConnectError),
    #[error("environment resolution failed: {0}")]
    Resolve(#[from] tonic::Status),
    #[error("environment resolution timed out: {0}")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("execution binding lacks required {0}")]
    Missing(&'static str),
    #[error("invalid environment definition: {0}")]
    Definition(#[from] EnvironmentDecodeError),
    #[error("invalid execution evidence: {0}")]
    Execution(#[from] ExecutionProtoError),
    #[error("execution binding mismatch: {0}")]
    Binding(#[from] EnvironmentBindingError),
    #[error("environment resolver returned a different requested binding")]
    Reference,
    #[error("recipe kernel pin differs from the resolved environment")]
    KernelPin,
    #[error("cannot read template metadata {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid template metadata JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid template metadata schema: {0}")]
    Schema(#[from] pplx_space_common_core::types::TemplateMetadataSchemaError),
}

#[derive(Debug)]
pub(super) struct BuildExecutionBinding {
    definition: EnvironmentDefinition,
    execution: ExecutionMetadata,
}

impl BuildExecutionBinding {
    /// Resolve within 30 seconds, including connection setup.
    pub(super) async fn resolve(
        reference: Option<EnvironmentReference>,
    ) -> Result<Self, BuildExecutionBindingError> {
        let response = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let mut node = pplx_space_node_mgr_client::connect().await?;
            Ok::<_, BuildExecutionBindingError>(
                node.resolve_execution_environment(ResolveExecutionEnvironmentRequest {
                    reference: reference.map(|value| value.to_string()),
                })
                .await?
                .into_inner(),
            )
        })
        .await??;
        Self::from_response(reference, response)
    }

    fn from_response(
        reference: Option<EnvironmentReference>,
        response: ResolveExecutionEnvironmentResponse,
    ) -> Result<Self, BuildExecutionBindingError> {
        let definition = EnvironmentDefinition::try_from(
            response
                .definition
                .as_ref()
                .ok_or(BuildExecutionBindingError::Missing("definition"))?,
        )?;
        if reference.is_some_and(|reference| reference != definition.reference()) {
            return Err(BuildExecutionBindingError::Reference);
        }
        let execution = ExecutionMetadata::try_from(
            response
                .execution
                .as_ref()
                .ok_or(BuildExecutionBindingError::Missing("execution"))?,
        )?;
        // A successful legacy RPC or a dropped field is not a support signal.
        if execution.environment() != Some(definition.reference()) {
            return Err(BuildExecutionBindingError::Missing(
                "verified environment reference",
            ));
        }
        let execution = definition.bind_execution(execution)?;
        Ok(Self {
            definition,
            execution,
        })
    }

    pub(super) fn bind(
        &self,
        execution: ExecutionMetadata,
    ) -> Result<ExecutionMetadata, BuildExecutionBindingError> {
        Ok(self.definition.bind_execution(execution)?)
    }

    pub(super) fn configure_scratch(
        &self,
        command: &mut tokio::process::Command,
        pin: Option<&super::GuestKernelPin>,
    ) -> Result<(), BuildExecutionBindingError> {
        use pplx_space_common_core::types::{BootProfile, ExecutionCompatibilityProfile};
        let BootProfile::LinuxDirectV1 { kernel } = &self.definition.descriptor().spec().boot;
        if pin.is_some_and(|pin| pin.version != kernel.version || pin.digest != kernel.digest) {
            return Err(BuildExecutionBindingError::KernelPin);
        }
        let ExecutionCompatibilityProfile::FirecrackerSnapshotV1 { state_format } =
            &self.definition.descriptor().spec().compatibility;
        // Values are resolved observations, never inherited selector ENV values.
        command.env("SPACE_SEED_RUNTIME", self.execution.runtime().kind.as_str());
        command.env(
            "SPACE_SEED_RUNTIME_VERSION",
            self.execution.runtime().version.as_str(),
        );
        command.env(
            "SPACE_SEED_STATE_FORMAT_VERSION",
            state_format.version.as_str(),
        );
        command.env("SPACE_SEED_KERNEL_VERSION", kernel.version.as_str());
        command.env("SPACE_SEED_KERNEL_DIGEST", kernel.digest.to_string());
        Ok(())
    }

    pub(super) fn bind_template(
        &self,
        metadata: &mut TemplateMetadata,
    ) -> Result<(), BuildExecutionBindingError> {
        let observed =
            metadata
                .execution_metadata()?
                .ok_or(BuildExecutionBindingError::Missing(
                    "template execution metadata",
                ))?;
        let bound = self.bind(observed.clone())?;
        metadata.set_execution_metadata(bound);
        Ok(())
    }
}

pub(super) fn read_metadata(path: &Path) -> Result<TemplateMetadata, BuildExecutionBindingError> {
    let bytes = std::fs::read(path).map_err(|source| BuildExecutionBindingError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests;
