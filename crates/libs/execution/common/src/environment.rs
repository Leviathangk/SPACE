//! Schema-v1 environment codec and content identity. This is not an execution
//! allowlist: consumers must resolve the reference against their approved catalog.

use aws_lc_rs::digest::{SHA256, digest};
use pplx_space_common_core::types::*;

use crate::{ExecutionProtoError, proto};

mod binding;
pub use binding::EnvironmentBindingError;

pub const ENVIRONMENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvironmentDefinition {
    reference: EnvironmentReference,
    descriptor: EnvironmentDescriptor,
}

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentDecodeError {
    #[error("unsupported execution environment schema {0}")]
    UnsupportedSchema(u32),
    #[error("execution environment is missing {0}")]
    Missing(&'static str),
    #[error("unsupported execution environment {field} value {value}")]
    UnsupportedValue { field: &'static str, value: i32 },
    #[error("invalid execution environment field: {0}")]
    Field(#[from] ExecutionProtoError),
    #[error("invalid execution environment: {0}")]
    Descriptor(#[from] EnvironmentDescriptorError),
    #[error("invalid environment digest: {0}")]
    Digest(#[from] DigestParseError),
    #[error("environment reference {claimed} does not match descriptor {computed}")]
    ReferenceMismatch {
        claimed: EnvironmentReference,
        computed: EnvironmentReference,
    },
}

impl EnvironmentDefinition {
    pub fn new(descriptor: EnvironmentDescriptor) -> Self {
        let hashed = digest(&SHA256, canonical_descriptor(&descriptor).as_bytes());
        let mut bytes = [0; 32];
        bytes.copy_from_slice(hashed.as_ref());
        Self {
            reference: EnvironmentReference::new(Digest::sha256(bytes)),
            descriptor,
        }
    }

    pub fn reference(&self) -> EnvironmentReference {
        self.reference
    }
    pub fn descriptor(&self) -> &EnvironmentDescriptor {
        &self.descriptor
    }

    fn check_reference(
        self,
        claimed: EnvironmentReference,
    ) -> Result<Self, EnvironmentDecodeError> {
        if claimed != self.reference {
            return Err(EnvironmentDecodeError::ReferenceMismatch {
                claimed,
                computed: self.reference,
            });
        }
        Ok(self)
    }
}

/// Fixed field order, ASCII keys and canonical scalar spellings, final newline.
/// Identifiers cannot contain newlines or '='. No wire/JSON bytes, observations,
/// labels, source URLs, or node-local paths participate. See docs/execution-environments.md.
pub fn canonical_descriptor(descriptor: &EnvironmentDescriptor) -> String {
    let spec = descriptor.spec();
    let RuntimeBinding::ManagedRelease(release) = &spec.runtime;
    let BootProfile::LinuxDirectV1 { kernel } = &spec.boot;
    let ExecutionCompatibilityProfile::FirecrackerSnapshotV1 { state_format } = &spec.compatibility;
    format!(
        concat!(
            "space.execution.environment\nschema={}\n",
            "host.os={}\nhost.arch={}\nbackend={}\nguest.os={}\nguest.arch={}\n",
            "runtime.provider=managed_release\nruntime.kind={}\nruntime.version={}\nruntime.digest={}\n",
            "runtime.target.os={}\nruntime.target.arch={}\nruntime.layout={}\n",
            "boot.profile=linux_direct_v1\nboot.kernel.kind={}\nboot.kernel.version={}\nboot.kernel.digest={}\n",
            "isolation={}\ncompatibility.profile=firecracker_snapshot_v1\n",
            "compatibility.state.format={}\ncompatibility.state.version={}\n"
        ),
        ENVIRONMENT_SCHEMA_VERSION,
        spec.host.os,
        spec.host.architecture,
        spec.backend,
        spec.guest.os,
        spec.guest.architecture,
        release.runtime.kind,
        release.runtime.version,
        release.digest,
        release.target.os,
        release.target.architecture,
        release.layout,
        kernel.kind,
        kernel.version,
        kernel.digest,
        spec.isolation,
        state_format.format,
        state_format.version
    )
}

fn required<T>(value: Option<T>, field: &'static str) -> Result<T, EnvironmentDecodeError> {
    value.ok_or(EnvironmentDecodeError::Missing(field))
}
fn unsupported(field: &'static str, value: i32) -> EnvironmentDecodeError {
    EnvironmentDecodeError::UnsupportedValue { field, value }
}

impl From<HostPlatform> for proto::HostPlatform {
    fn from(value: HostPlatform) -> Self {
        Self {
            os: match value.os {
                HostOperatingSystem::Linux => proto::HostOperatingSystem::Linux,
                HostOperatingSystem::Windows => proto::HostOperatingSystem::Windows,
                HostOperatingSystem::Macos => proto::HostOperatingSystem::Macos,
            } as i32,
            architecture: proto::CpuArchitecture::from(value.architecture) as i32,
        }
    }
}
impl TryFrom<&proto::HostPlatform> for HostPlatform {
    type Error = EnvironmentDecodeError;
    fn try_from(value: &proto::HostPlatform) -> Result<Self, Self::Error> {
        Ok(Self {
            os: match proto::HostOperatingSystem::try_from(value.os) {
                Ok(proto::HostOperatingSystem::Linux) => HostOperatingSystem::Linux,
                Ok(proto::HostOperatingSystem::Windows) => HostOperatingSystem::Windows,
                Ok(proto::HostOperatingSystem::Macos) => HostOperatingSystem::Macos,
                _ => return Err(unsupported("host OS", value.os)),
            },
            architecture: proto::CpuArchitecture::try_from(value.architecture)
                .map_err(|_| unsupported("host architecture", value.architecture))?
                .try_into()?,
        })
    }
}

impl From<&EnvironmentDefinition> for proto::EnvironmentDefinition {
    fn from(value: &EnvironmentDefinition) -> Self {
        let spec = value.descriptor.spec();
        let RuntimeBinding::ManagedRelease(release) = &spec.runtime;
        let BootProfile::LinuxDirectV1 { kernel } = &spec.boot;
        let ExecutionCompatibilityProfile::FirecrackerSnapshotV1 { state_format } =
            &spec.compatibility;
        Self {
            schema_version: ENVIRONMENT_SCHEMA_VERSION,
            reference: value.reference.to_string(),
            descriptor: Some(proto::EnvironmentDescriptor {
                host: Some(spec.host.into()),
                guest: Some(spec.guest.into()),
                runtime: Some(proto::RuntimeBinding {
                    provider: Some(proto::runtime_binding::Provider::ManagedRelease(
                        proto::ManagedRuntimeRelease {
                            runtime: Some((&release.runtime).into()),
                            digest: release.digest.to_string(),
                            target: Some(release.target.into()),
                            layout: match release.layout {
                                RuntimePayloadLayout::FirecrackerReleaseV1 => {
                                    proto::RuntimePayloadLayout::FirecrackerReleaseV1
                                }
                            } as i32,
                        },
                    )),
                }),
                backend: match spec.backend {
                    HypervisorBackend::Kvm => proto::HypervisorBackend::Kvm,
                } as i32,
                isolation: match spec.isolation {
                    IsolationClass::HardwareVm => proto::IsolationClass::HardwareVm,
                } as i32,
                boot: Some(proto::BootProfile {
                    profile: Some(proto::boot_profile::Profile::LinuxDirectV1(
                        proto::LinuxDirectBootV1 {
                            kernel: Some(proto::EnvironmentComponent {
                                kind: proto::ArtifactComponentKind::from(kernel.kind) as i32,
                                version: kernel.version.to_string(),
                                digest: kernel.digest.to_string(),
                            }),
                        },
                    )),
                }),
                compatibility: Some(proto::ExecutionCompatibilityProfile {
                    profile: Some(
                        proto::execution_compatibility_profile::Profile::FirecrackerSnapshotV1(
                            proto::FirecrackerSnapshotCompatibilityV1 {
                                state_format: Some(state_format.into()),
                            },
                        ),
                    ),
                }),
            }),
        }
    }
}

impl TryFrom<&proto::EnvironmentDefinition> for EnvironmentDefinition {
    type Error = EnvironmentDecodeError;
    fn try_from(value: &proto::EnvironmentDefinition) -> Result<Self, Self::Error> {
        if value.schema_version != ENVIRONMENT_SCHEMA_VERSION {
            return Err(EnvironmentDecodeError::UnsupportedSchema(
                value.schema_version,
            ));
        }
        let spec = required(value.descriptor.as_ref(), "descriptor")?;
        let proto::runtime_binding::Provider::ManagedRelease(release) = required(
            required(spec.runtime.as_ref(), "runtime")?
                .provider
                .as_ref(),
            "runtime provider",
        )?;
        let proto::boot_profile::Profile::LinuxDirectV1(boot) = required(
            required(spec.boot.as_ref(), "boot")?.profile.as_ref(),
            "boot profile",
        )?;
        let kernel = required(boot.kernel.as_ref(), "boot kernel")?;
        let proto::execution_compatibility_profile::Profile::FirecrackerSnapshotV1(compatibility) =
            required(
                required(spec.compatibility.as_ref(), "compatibility")?
                    .profile
                    .as_ref(),
                "compatibility profile",
            )?;
        let descriptor = EnvironmentSpec {
            host: required(spec.host.as_ref(), "host")?.try_into()?,
            guest: required(spec.guest.as_ref(), "guest")?.try_into()?,
            backend: match proto::HypervisorBackend::try_from(spec.backend) {
                Ok(proto::HypervisorBackend::Kvm) => HypervisorBackend::Kvm,
                _ => return Err(unsupported("backend", spec.backend)),
            },
            isolation: match proto::IsolationClass::try_from(spec.isolation) {
                Ok(proto::IsolationClass::HardwareVm) => IsolationClass::HardwareVm,
                _ => return Err(unsupported("isolation", spec.isolation)),
            },
            runtime: RuntimeBinding::ManagedRelease(ManagedRuntimeRelease {
                runtime: required(release.runtime.as_ref(), "release runtime")?.try_into()?,
                digest: release.digest.parse()?,
                target: required(release.target.as_ref(), "release target")?.try_into()?,
                layout: match proto::RuntimePayloadLayout::try_from(release.layout) {
                    Ok(proto::RuntimePayloadLayout::FirecrackerReleaseV1) => {
                        RuntimePayloadLayout::FirecrackerReleaseV1
                    }
                    _ => return Err(unsupported("release layout", release.layout)),
                },
            }),
            boot: BootProfile::LinuxDirectV1 {
                kernel: ComponentIdentity {
                    kind: proto::ArtifactComponentKind::try_from(kernel.kind)
                        .map_err(|_| unsupported("component kind", kernel.kind))?
                        .try_into()?,
                    version: kernel.version.parse().map_err(ExecutionProtoError::from)?,
                    digest: kernel.digest.parse()?,
                },
            },
            compatibility: ExecutionCompatibilityProfile::FirecrackerSnapshotV1 {
                state_format: required(compatibility.state_format.as_ref(), "state format")?
                    .try_into()?,
            },
        }
        .try_into()?;
        Self::new(descriptor).check_reference(value.reference.parse()?)
    }
}

#[cfg(feature = "serde")]
mod document;
#[cfg(feature = "serde")]
pub use document::EnvironmentDocumentError;

#[cfg(test)]
mod tests;
