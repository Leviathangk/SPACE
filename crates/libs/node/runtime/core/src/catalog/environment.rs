//! Compiled qualification for the first environment descriptor. Artifact
//! installation approval alone does not approve an environment for execution.

use pplx_space_common_core::types::*;
use pplx_space_execution_common::environment::EnvironmentDefinition;

use super::InitialEnvironment;

#[derive(Debug, thiserror::Error)]
pub enum InitialEnvironmentDefinitionError {
    #[error("no qualified initial environment descriptor for architecture {0}")]
    UnsupportedArchitecture(Arch),
    #[error("invalid compiled initial environment descriptor: {0}")]
    Descriptor(#[from] EnvironmentDescriptorError),
    #[error("invalid compiled initial state format: {0}")]
    StateFormat(#[from] StateFormatNameParseError),
    #[error("invalid compiled initial state format version: {0}")]
    StateFormatVersion(#[from] StateFormatVersionParseError),
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveEnvironmentError {
    #[error(transparent)]
    Definition(#[from] InitialEnvironmentDefinitionError),
    #[error("environment {0} is not the compiled initial environment")]
    UnknownReference(EnvironmentReference),
}

impl InitialEnvironment {
    /// Phase-one qualification covers Linux/x86_64 E0 only. Existing artifact
    /// pins for other architectures retain their installation meaning.
    pub fn definition(&self) -> Result<EnvironmentDefinition, InitialEnvironmentDefinitionError> {
        if self.release.architecture != Arch::X86_64 {
            return Err(InitialEnvironmentDefinitionError::UnsupportedArchitecture(
                self.release.architecture,
            ));
        }
        let host = HostPlatform {
            os: HostOperatingSystem::Linux,
            architecture: self.release.architecture,
        };
        let descriptor = EnvironmentSpec {
            host,
            guest: GuestPlatform {
                os: GuestOperatingSystem::Linux,
                architecture: self.kernel.architecture,
            },
            runtime: RuntimeBinding::ManagedRelease(ManagedRuntimeRelease {
                runtime: self.release.release.clone(),
                digest: self.release.digest(),
                target: host,
                layout: RuntimePayloadLayout::FirecrackerReleaseV1,
            }),
            backend: HypervisorBackend::Kvm,
            boot: BootProfile::LinuxDirectV1 {
                kernel: ComponentIdentity {
                    kind: ArtifactComponentKind::Kernel,
                    version: self.kernel.version.clone(),
                    digest: self.kernel.sha256,
                },
            },
            isolation: IsolationClass::HardwareVm,
            compatibility: ExecutionCompatibilityProfile::FirecrackerSnapshotV1 {
                state_format: StateFormatIdentity {
                    format: FIRECRACKER_STATE_FORMAT.parse()?,
                    version: INITIAL_FIRECRACKER_STATE_FORMAT_VERSION.parse()?,
                },
            },
        }
        .try_into()?;
        Ok(EnvironmentDefinition::new(descriptor))
    }

    /// Resolve by complete environment identity, never by an artifact version
    /// or by whatever happens to be installed. This does not select a default.
    pub fn resolve_definition(
        &self,
        reference: EnvironmentReference,
    ) -> Result<EnvironmentDefinition, ResolveEnvironmentError> {
        let definition = self.definition()?;
        if definition.reference() != reference {
            return Err(ResolveEnvironmentError::UnknownReference(reference));
        }
        Ok(definition)
    }
}

#[cfg(test)]
mod tests;
