//! Template-owned boot startup. PID1 runs the application; spaced only gates
//! public boot readiness. The internal agent-ready flag remains independent.

use crate::SpacedGuestService;
#[cfg(target_os = "linux")]
use crate::identity::GuestAccount;
#[cfg(target_os = "linux")]
use pplx_space_common_core::types::{EnvKey, EnvMap, EnvValue};
use pplx_space_common_core::types::{
    MAX_TEMPLATE_RUNTIME_BYTES, TEMPLATE_RUNTIME_BUILD_MASK, TEMPLATE_RUNTIME_PATH,
    TEMPLATE_RUNTIME_UNIT, TemplateRuntime,
};
use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::Instant;
use tonic::Status;

#[cfg(target_os = "linux")]
mod runner;
#[cfg(target_os = "linux")]
pub use runner::run_runtime_launcher;
#[cfg(not(target_os = "linux"))]
pub fn run_runtime_launcher() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "template runtimes require Linux",
    ))
}

pub const OUTPUT_PATH: &str = "/run/space-runtime/output.log";
pub const SYSTEMD_UNIT: &[u8] = include_bytes!("template_runtime/space-runtime.service");
#[cfg(target_os = "linux")]
const OUTPUT_LIMIT: u64 = 1024 * 1024;

#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
struct InvocationConfig {
    declaration: TemplateRuntime,
    env: EnvMap,
}

#[derive(Clone, Copy, Debug, thiserror::Error)]
enum RuntimeFailure {
    #[cfg(target_os = "linux")]
    #[error("invalid template runtime configuration")]
    Configuration,
    #[error("template runtime backend is unavailable or incompatible")]
    Backend,
}

fn read_declaration() -> io::Result<Option<TemplateRuntime>> {
    read_regular(Path::new(TEMPLATE_RUNTIME_PATH), MAX_TEMPLATE_RUNTIME_BYTES)?
        .map(|bytes| {
            TemplateRuntime::from_bytes(&bytes)
                .map_err(|_| io::Error::other("invalid template runtime declaration"))
        })
        .transpose()
}

fn builder_boot(cmdline: &str) -> bool {
    cmdline
        .split_whitespace()
        .any(|argument| argument == TEMPLATE_RUNTIME_BUILD_MASK)
}

fn application_required() -> io::Result<bool> {
    if let Ok(cmdline) = std::fs::read_to_string("/proc/cmdline")
        && builder_boot(&cmdline)
    {
        return Ok(false);
    }
    Ok(read_declaration()?.is_some())
}

impl SpacedGuestService {
    pub(crate) async fn wait_template_runtime_boot_ready(
        &self,
        deadline: Instant,
    ) -> Result<bool, Status> {
        if self.inner.runtime_boot_ready.load(Ordering::Acquire) {
            return Ok(true);
        }
        let required = tokio::task::spawn_blocking(application_required)
            .await
            .map_err(|_| Status::internal("template runtime inspection failed"))?
            .map_err(|_| Status::failed_precondition("invalid template runtime configuration"))?;
        if !required {
            self.inner.runtime_boot_ready.store(true, Ordering::Release);
            return Ok(true);
        }
        loop {
            if Instant::now() >= deadline {
                return Ok(false);
            }
            let observed = tokio::time::timeout_at(deadline, systemctl(&["show", TEMPLATE_RUNTIME_UNIT,
                "--property=LoadState,ActiveState,NotifyAccess,MainPID,InactiveExitTimestampMonotonic"]))
                .await;
            let output = match observed {
                Err(_) => return Ok(false),
                Ok(Err(_)) => {
                    return Err(Status::failed_precondition(
                        "template runtime backend unavailable",
                    ));
                }
                Ok(Ok(output)) => output,
            };
            match observe_boot(&output)? {
                BootObservation::Ready => {
                    self.inner.runtime_boot_ready.store(true, Ordering::Release);
                    return Ok(true);
                }
                BootObservation::Starting => {
                    tokio::time::sleep_until(
                        deadline.min(Instant::now() + Duration::from_millis(100)),
                    )
                    .await
                }
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum BootObservation {
    Starting,
    Ready,
}

fn observe_boot(output: &[u8]) -> Result<BootObservation, Status> {
    let output = std::str::from_utf8(output)
        .map_err(|_| Status::failed_precondition("invalid runtime backend state"))?;
    let properties: BTreeMap<_, _> = output
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect();
    let failed = || Status::failed_precondition("template runtime startup failed");
    if properties.get("LoadState") != Some(&"loaded") {
        return Err(failed());
    }
    match properties.get("ActiveState").copied() {
        Some("active")
            if properties.get("NotifyAccess") == Some(&"none")
                && properties
                    .get("MainPID")
                    .and_then(|pid| pid.parse::<u32>().ok())
                    .is_some_and(|pid| pid > 0) =>
        {
            Ok(BootObservation::Ready)
        }
        Some("failed") => Err(failed()),
        Some("inactive")
            if properties
                .get("InactiveExitTimestampMonotonic")
                .is_some_and(|value| *value != "0") =>
        {
            Err(failed())
        }
        Some("active" | "inactive" | "activating" | "deactivating") => {
            Ok(BootObservation::Starting)
        }
        _ => Err(failed()),
    }
}

/// Construction-only inspection: stdout is exact declaration bytes or absence.
/// The mask is checked before callers may mutate an inherited runtime template.
pub fn inspect_template_runtime() -> io::Result<Vec<u8>> {
    if !builder_boot(&std::fs::read_to_string("/proc/cmdline")?) {
        return Err(io::Error::other(
            "builder runtime mask missing; upgrade the sandbox manager",
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        // Masked units cannot expose service properties. Inspect the installed
        // platform unit while retaining the kernel mask for the whole build.
        let version = systemctl(&["--version"])
            .await
            .map_err(|_| io::Error::other("systemd unavailable"))?;
        if std::str::from_utf8(&version)
            .ok()
            .and_then(|text| text.split_whitespace().nth(1))
            .and_then(|value| value.parse::<u32>().ok())
            .is_none_or(|version| version < 254)
        {
            return Err(io::Error::other("runtime requires systemd 254 or newer"));
        }
        systemctl(&["daemon-reload"])
            .await
            .map_err(|_| io::Error::other("cannot load runtime backend configuration"))?;
        let wants = systemctl(&["show", "space.target", "--property=Wants", "--value"])
            .await
            .map_err(|_| io::Error::other("platform target unavailable"))?;
        if !String::from_utf8_lossy(&wants)
            .split_whitespace()
            .any(|unit| unit == TEMPLATE_RUNTIME_UNIT)
        {
            return Err(io::Error::other(
                "platform target does not start runtime; rebuild base",
            ));
        }
        Ok::<_, io::Error>(())
    })?;
    let unit = read_regular(
        Path::new("/etc/systemd/system/space-runtime.service"),
        16 * 1024,
    )?
    .ok_or_else(|| io::Error::other("runtime unit missing; rebuild the base"))?;
    // A single platform-owned unit has an exact contract, not tenant-authored
    // policy. The builder rejects alterations instead of interpreting unit text.
    if unit != SYSTEMD_UNIT {
        return Err(io::Error::other(
            "unsupported runtime unit; rebuild the base",
        ));
    }
    reject_legacy()?;
    let bytes = read_regular(Path::new(TEMPLATE_RUNTIME_PATH), MAX_TEMPLATE_RUNTIME_BYTES)?;
    if let Some(bytes) = &bytes {
        TemplateRuntime::from_bytes(bytes)
            .map_err(|_| io::Error::other("invalid declaration; use SetRuntime or ClearRuntime"))?;
    }
    Ok(bytes.unwrap_or_default())
}

/// Do not follow symlinks or block opening FIFOs/devices. Only ENOENT is absence.
fn read_regular(path: &Path, limit: usize) -> io::Result<Option<Vec<u8>>> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime configuration is not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "runtime configuration exceeds size limit",
        ));
    }
    Ok(Some(bytes))
}

async fn systemctl(args: &[&str]) -> Result<Vec<u8>, RuntimeFailure> {
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;
    let mut child = Command::new("/usr/bin/systemctl")
        .args(args)
        .env("SYSTEMD_PAGER", "")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| RuntimeFailure::Backend)?;
    let mut output = Vec::new();
    let mut stdout = child
        .stdout
        .take()
        .ok_or(RuntimeFailure::Backend)?
        .take(16 * 1024 + 1);
    let operation = async {
        stdout
            .read_to_end(&mut output)
            .await
            .map_err(|_| RuntimeFailure::Backend)?;
        if output.len() > 16 * 1024 {
            return Err(RuntimeFailure::Backend);
        }
        let status = child.wait().await.map_err(|_| RuntimeFailure::Backend)?;
        if !status.success() {
            return Err(RuntimeFailure::Backend);
        }
        Ok(output)
    };
    tokio::time::timeout(Duration::from_secs(2), operation)
        .await
        .map_err(|_| RuntimeFailure::Backend)?
}

#[cfg(target_os = "linux")]
fn resolve_config(declaration: TemplateRuntime) -> Result<InvocationConfig, RuntimeFailure> {
    let account = GuestAccount::resolve(declaration.user().clone())
        .map_err(|_| RuntimeFailure::Configuration)?;
    let mut env: BTreeMap<EnvKey, EnvValue> = [
        (
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned(),
        ),
        ("HOME", account.home().to_string_lossy().into_owned()),
        ("USER", account.name().as_str().to_owned()),
        ("LOGNAME", account.name().as_str().to_owned()),
        ("SHELL", account.shell().to_string_lossy().into_owned()),
    ]
    .into_iter()
    .map(|(key, value)| {
        Ok((
            EnvKey::parse(key).map_err(|_| RuntimeFailure::Configuration)?,
            EnvValue::parse(value).map_err(|_| RuntimeFailure::Configuration)?,
        ))
    })
    .collect::<Result<_, RuntimeFailure>>()?;
    env.extend(declaration.env().as_map().clone());
    for key in ["NOTIFY_SOCKET", "WATCHDOG_PID", "WATCHDOG_USEC"] {
        env.remove(&EnvKey::parse(key).map_err(|_| RuntimeFailure::Configuration)?);
    }
    Ok(InvocationConfig {
        declaration,
        env: EnvMap::new(env).map_err(|_| RuntimeFailure::Configuration)?,
    })
}

fn reject_legacy() -> io::Result<()> {
    for directory in [
        "/etc/systemd/system",
        "/run/systemd/system",
        "/usr/lib/systemd/system",
        "/lib/systemd/system",
    ] {
        let path = Path::new(directory);
        let overrides = path.join("space-runtime.service.d");
        match std::fs::read_dir(overrides) {
            Ok(mut entries) => {
                if entries.next().is_some() {
                    return Err(io::Error::other("runtime unit overrides are unsupported"));
                }
            }
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
        for name in ["e2b-template-start.service", "space-start-cmd.service"] {
            if path.join(name).symlink_metadata().is_ok() {
                return Err(io::Error::other("rebuild legacy startup from recipe"));
            }
        }
        let entries = match std::fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_name().as_encoded_bytes().ends_with(b".wants")
                || entry.file_name().as_encoded_bytes().ends_with(b".requires")
            {
                for name in ["e2b-template-start.service", "space-start-cmd.service"] {
                    if entry.path().join(name).symlink_metadata().is_ok() {
                        return Err(io::Error::other("rebuild legacy startup from recipe"));
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
