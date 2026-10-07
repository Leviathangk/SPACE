//! Guest-owned template startup intent. The rootfs is the only durable owner.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::{EnvMap, GuestPath, GuestUserName, ProcessCommand};

pub const TEMPLATE_RUNTIME_PATH: &str = "/etc/space/runtime.json";
pub const TEMPLATE_RUNTIME_BUILD_MASK: &str = "systemd.mask=space-runtime.service";
pub const TEMPLATE_RUNTIME_UNIT: &str = "space-runtime.service";
pub const MAX_TEMPLATE_RUNTIME_BYTES: usize = 256 * 1024;
pub const TEMPLATE_RUNTIME_TIMEOUT: Duration = Duration::from_secs(60);

/// Commands and environment values are deliberately redacted in Debug.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Declaration", into = "Declaration")]
pub struct TemplateRuntime {
    command: ProcessCommand,
    user: GuestUserName,
    working_directory: GuestPath,
    env: EnvMap,
    readiness: RuntimeReadiness,
    startup_timeout: Duration,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeReadiness {
    Launch,
    Exec(ProcessCommand),
}

#[derive(Debug, thiserror::Error)]
pub enum TemplateRuntimeError {
    #[error("runtime declaration exceeds the size limit")]
    TooLarge,
    // serde errors can include tenant input. Keep it out of generic error logs.
    #[error("invalid runtime declaration JSON or schema")]
    Json,
    #[error("unsupported runtime declaration schema")]
    Schema,
    #[error("invalid runtime command")]
    Command,
    #[error("runtime working_directory must be absolute")]
    WorkingDirectory,
    #[error("runtime startup_timeout_secs must be between 1 and 60")]
    Timeout,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    schema_version: u32,
    command: Vec<String>,
    user: GuestUserName,
    working_directory: String,
    #[serde(default)]
    env: EnvMap,
    readiness: Readiness,
    startup_timeout_secs: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Readiness {
    Launch,
    Exec { command: Vec<String> },
}

impl TryFrom<Declaration> for TemplateRuntime {
    type Error = TemplateRuntimeError;

    fn try_from(value: Declaration) -> Result<Self, Self::Error> {
        if value.schema_version != 1 {
            return Err(TemplateRuntimeError::Schema);
        }
        if !value.working_directory.starts_with('/') {
            return Err(TemplateRuntimeError::WorkingDirectory);
        }
        if !(1..=TEMPLATE_RUNTIME_TIMEOUT.as_secs()).contains(&value.startup_timeout_secs) {
            return Err(TemplateRuntimeError::Timeout);
        }
        Ok(Self {
            command: ProcessCommand::new(value.command)
                .map_err(|_| TemplateRuntimeError::Command)?,
            user: value.user,
            working_directory: GuestPath::parse(value.working_directory)
                .map_err(|_| TemplateRuntimeError::WorkingDirectory)?,
            env: value.env,
            readiness: match value.readiness {
                Readiness::Launch => RuntimeReadiness::Launch,
                Readiness::Exec { command } => RuntimeReadiness::Exec(
                    ProcessCommand::new(command).map_err(|_| TemplateRuntimeError::Command)?,
                ),
            },
            startup_timeout: Duration::from_secs(value.startup_timeout_secs),
        })
    }
}

impl From<TemplateRuntime> for Declaration {
    fn from(value: TemplateRuntime) -> Self {
        Self {
            schema_version: 1,
            command: value.command.argv().to_vec(),
            user: value.user,
            working_directory: value.working_directory.expose().to_owned(),
            env: value.env,
            readiness: match value.readiness {
                RuntimeReadiness::Launch => Readiness::Launch,
                RuntimeReadiness::Exec(command) => Readiness::Exec {
                    command: command.argv().to_vec(),
                },
            },
            startup_timeout_secs: value.startup_timeout.as_secs(),
        }
    }
}

impl TemplateRuntime {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, TemplateRuntimeError> {
        if bytes.len() > MAX_TEMPLATE_RUNTIME_BYTES {
            return Err(TemplateRuntimeError::TooLarge);
        }
        serde_json::from_slice(bytes).map_err(|_| TemplateRuntimeError::Json)
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, TemplateRuntimeError> {
        let bytes = serde_json::to_vec(self).map_err(|_| TemplateRuntimeError::Json)?;
        if bytes.len() > MAX_TEMPLATE_RUNTIME_BYTES {
            return Err(TemplateRuntimeError::TooLarge);
        }
        Ok(bytes)
    }

    pub fn command(&self) -> &ProcessCommand {
        &self.command
    }
    pub fn user(&self) -> &GuestUserName {
        &self.user
    }
    pub fn working_directory(&self) -> &GuestPath {
        &self.working_directory
    }
    pub fn env(&self) -> &EnvMap {
        &self.env
    }
    pub fn readiness(&self) -> &RuntimeReadiness {
        &self.readiness
    }
    pub fn startup_timeout(&self) -> Duration {
        self.startup_timeout
    }
}

#[cfg(test)]
mod tests;
