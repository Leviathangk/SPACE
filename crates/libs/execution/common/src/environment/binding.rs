//! Match execution observations to an already approved environment. Binary
//! integrity and host prerequisites remain the node resolver's responsibility.
use super::*;

#[derive(Debug, thiserror::Error)]
pub enum EnvironmentBindingError {
    #[error("execution names environment {actual}, expected {expected}")]
    Reference {
        actual: EnvironmentReference,
        expected: EnvironmentReference,
    },
    #[error("execution runtime or guest differs from the environment")]
    RuntimeOrGuest,
    #[error("execution state format differs from the environment")]
    StateFormat,
    #[error("execution boot components differ from the environment")]
    BootComponents,
}

impl EnvironmentDefinition {
    /// Bind observations only after checking their complete supported shape.
    /// Kernel bytes take precedence over historical directory-name aliases,
    /// matching the existing runtime component resolver's identity semantics.
    pub fn bind_execution(
        &self,
        execution: ExecutionMetadata,
    ) -> Result<ExecutionMetadata, EnvironmentBindingError> {
        if let Some(actual) = execution.environment()
            && actual != self.reference()
        {
            return Err(EnvironmentBindingError::Reference {
                actual,
                expected: self.reference(),
            });
        }
        let spec = self.descriptor().spec();
        let RuntimeBinding::ManagedRelease(release) = &spec.runtime;
        if execution.runtime() != &release.runtime || execution.guest() != spec.guest {
            return Err(EnvironmentBindingError::RuntimeOrGuest);
        }
        let ExecutionCompatibilityProfile::FirecrackerSnapshotV1 { state_format } =
            &spec.compatibility;
        if execution.state_format() != Some(state_format) {
            return Err(EnvironmentBindingError::StateFormat);
        }
        let BootProfile::LinuxDirectV1 { kernel } = &spec.boot;
        let [observed] = execution.components() else {
            return Err(EnvironmentBindingError::BootComponents);
        };
        if observed.kind != kernel.kind
            || observed.digest != Some(kernel.digest)
            || !observed
                .role
                .as_ref()
                .is_some_and(|role| role.as_str() == "boot")
        {
            return Err(EnvironmentBindingError::BootComponents);
        }
        Ok(execution.with_environment(self.reference()))
    }
}

#[cfg(test)]
mod tests;
