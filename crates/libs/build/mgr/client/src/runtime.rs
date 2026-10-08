//! Shared native recipe boundary. The core declaration owns validation.
use crate::proto::recipe::{SetRuntime, set_runtime};
use pplx_space_common_core::types::{RuntimeReadiness, TemplateRuntime, TemplateRuntimeError};

impl std::fmt::Debug for SetRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetRuntime").finish_non_exhaustive()
    }
}
impl std::fmt::Debug for set_runtime::ExecReady {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecReady").finish_non_exhaustive()
    }
}

pub fn encode(runtime: &TemplateRuntime) -> SetRuntime {
    SetRuntime {
        schema_version: 1,
        command: runtime.command().argv().to_vec(),
        user: runtime.user().as_str().to_owned(),
        working_directory: runtime.working_directory().expose().to_owned(),
        env: runtime
            .env()
            .as_map()
            .iter()
            .map(|(key, value)| (key.as_str().to_owned(), value.expose().to_owned()))
            .collect(),
        readiness: Some(match runtime.readiness() {
            RuntimeReadiness::Launch => set_runtime::Readiness::Launch(set_runtime::LaunchReady {}),
            RuntimeReadiness::Exec(command) => {
                set_runtime::Readiness::Exec(set_runtime::ExecReady {
                    command: command.argv().to_vec(),
                })
            }
        }),
        startup_timeout_secs: runtime.startup_timeout().as_secs(),
    }
}

pub fn decode(runtime: SetRuntime) -> Result<TemplateRuntime, TemplateRuntimeError> {
    let readiness = match runtime.readiness.ok_or(TemplateRuntimeError::Command)? {
        set_runtime::Readiness::Launch(_) => serde_json::json!({"type": "launch"}),
        set_runtime::Readiness::Exec(probe) => {
            serde_json::json!({"type": "exec", "command": probe.command})
        }
    };
    let result: TemplateRuntime = serde_json::from_value(serde_json::json!({
        "schema_version": runtime.schema_version, "command": runtime.command,
        "user": runtime.user, "working_directory": runtime.working_directory,
        "env": runtime.env, "readiness": readiness,
        "startup_timeout_secs": runtime.startup_timeout_secs,
    }))
    .map_err(|_| TemplateRuntimeError::Json)?;
    result.canonical_bytes()?;
    Ok(result)
}
