//! Run explicitly in a disposable systemd container with the native runtime
//! unit and built spaced-svc installed. The marker prevents accidental host use.
//! This destructive acceptance harness is an example binary, not an ignored
//! workspace test: CI deliberately executes every ignored test.
#![allow(clippy::unwrap_used)] // Assertion-based disposable acceptance fixture.

use pplx_space_spaced_common::proto::bridge_guest::meta_server::Meta;
use pplx_space_spaced_common::proto::bridge_guest::{ConfigureProcessEnvRequest, WaitReadyRequest};
use pplx_space_spaced_guest_server::{SpacedGuestService, SpacedGuestServiceConfig};
use std::collections::HashMap;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tonic::{Request, Status};

fn service() -> SpacedGuestService {
    let service = SpacedGuestService::with_config(
        CancellationToken::new(),
        SpacedGuestServiceConfig {
            runtime_state_directory: Some("/run/space-runtime".into()),
            ..SpacedGuestServiceConfig::default()
        },
    );
    service.ready();
    service
}

async fn ctl(args: &[&str]) -> String {
    let output = tokio::process::Command::new("systemctl")
        .args(args)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "systemctl {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

async fn property(name: &str) -> String {
    ctl(&[
        "show",
        "space-runtime.service",
        "--value",
        "--property",
        name,
    ])
    .await
}

async fn reset() {
    ctl(&["stop", "space-runtime.service", "space.target"]).await;
    ctl(&["reset-failed"]).await;
    for path in [
        "/etc/space/runtime.json",
        "/run/runtime-probes",
        "/run/runtime-ready",
        "/run/runtime-env",
        "/run/space-runtime/boot-ready",
        "/run/space-runtime/output.log",
        "/run/space-runtime/output.previous.log",
    ] {
        let _ = std::fs::remove_file(path);
    }
    // Release inactive invocation evidence to emulate a fresh PID1 boot.
    ctl(&["daemon-reload"]).await;
}

fn declaration(command: &[&str], probe: Option<&str>) {
    let readiness = probe.map_or(
        serde_json::json!({"type":"launch"}),
        |probe| serde_json::json!({"type":"exec","command":["/bin/sh","-c",probe]}),
    );
    let data = serde_json::json!({"schema_version":1,"command":command,"user":"root",
        "working_directory":"/","env":{"RUNTIME_TEST":"template"},
        "readiness":readiness,"startup_timeout_secs":30});
    std::fs::create_dir_all("/etc/space").unwrap();
    std::fs::write(
        "/etc/space/runtime.json",
        serde_json::to_vec(&data).unwrap(),
    )
    .unwrap();
}

async fn boot() {
    // Starting the platform target, without an application-specific command,
    // must enqueue native application startup.
    ctl(&["start", "--no-block", "space.target"]).await;
}

async fn ready(service: &SpacedGuestService, timeout: u64) -> Result<bool, Status> {
    service
        .wait_ready(Request::new(WaitReadyRequest {
            timeout: Some(prost_types::Duration {
                seconds: timeout as i64,
                nanos: 0,
            }),
        }))
        .await
        .map(|response| response.into_inner().ready)
}

#[tokio::main]
async fn main() {
    assert!(
        std::path::Path::new("/run/space-runtime-test-container").is_file(),
        "refusing to change host units"
    );
    reset().await;
    boot().await;
    assert!(ready(&service(), 5).await.unwrap());
    assert_eq!(property("MainPID").await, "0");
    // Absence is also a boot decision: later tenant edits cannot block resume.
    declaration(&["/nonexistent-runtime-app"], None);
    assert!(ready(&service(), 2).await.unwrap());

    reset().await;
    declaration(&["/usr/local/bin/spaced-svc", "--hold-cgroup"], None);
    boot().await;
    let guest = service();
    // A PID1 query failure is retryable; it must not become a permanent startup
    // verdict. This replacement is confined to the marker-guarded container.
    std::fs::rename("/usr/bin/systemctl", "/usr/bin/systemctl.real").unwrap();
    std::fs::write("/usr/bin/systemctl", "#!/bin/sh\nexit 1\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions("/usr/bin/systemctl", std::fs::Permissions::from_mode(0o755)).unwrap();
    let transient = ready(&guest, 5).await;
    std::fs::rename("/usr/bin/systemctl.real", "/usr/bin/systemctl").unwrap();
    assert_eq!(transient.unwrap_err().code(), tonic::Code::Unavailable);
    assert!(ready(&guest, 5).await.unwrap());
    assert_eq!(property("NotifyAccess").await, "none");
    let pid = property("MainPID").await;
    assert!(
        std::fs::read_link(format!("/proc/{pid}/exe"))
            .unwrap()
            .ends_with("spaced-svc")
    );
    let invocation = property("InvocationID").await;
    for operation in ["daemon-reload", "daemon-reexec"] {
        ctl(&[operation]).await;
        assert_eq!(property("InvocationID").await, invocation);
        assert_eq!(property("MainPID").await, pid);
        assert_eq!(property("NotifyAccess").await, "none");
    }
    ctl(&[
        "kill",
        "--kill-whom=main",
        "--signal=TERM",
        "space-runtime.service",
    ])
    .await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(property("NRestarts").await, "0");
    // Boot readiness survives agent loss even when the app has since exited.
    assert!(ready(&guest, 2).await.unwrap());
    assert!(ready(&service(), 2).await.unwrap());

    reset().await;
    declaration(
        &[
            "/bin/sh",
            "-c",
            "printf '%s' \"$RUNTIME_TEST\" > /run/runtime-env; exec /bin/sleep infinity",
        ],
        Some("echo probe-output; echo probe >> /run/runtime-probes; test -f /run/runtime-ready"),
    );
    let guest = service();
    guest
        .configure_process_env(Request::new(ConfigureProcessEnvRequest {
            env: HashMap::from([("RUNTIME_TEST".into(), "sandbox".into())]),
        }))
        .await
        .unwrap();
    boot().await;
    assert!(!ready(&guest, 1).await.unwrap());
    assert_eq!(
        std::fs::read_to_string("/run/runtime-env").unwrap(),
        "template"
    );
    let pid = property("MainPID").await;
    std::fs::write("/run/runtime-ready", b"ready").unwrap();
    assert!(ready(&guest, 5).await.unwrap());
    assert!(
        std::fs::read_to_string("/run/space-runtime/output.log")
            .unwrap()
            .contains("probe-output")
    );
    let probes = std::fs::read_to_string("/run/runtime-probes").unwrap();
    assert!(ready(&guest, 5).await.unwrap());
    assert_eq!(
        std::fs::read_to_string("/run/runtime-probes").unwrap(),
        probes
    );
    let recovered = service();
    assert!(ready(&recovered, 5).await.unwrap());
    assert_eq!(property("MainPID").await, pid);
    assert_eq!(
        std::fs::read_to_string("/run/runtime-probes").unwrap(),
        probes
    );
    ctl(&[
        "kill",
        "--kill-whom=main",
        "--signal=KILL",
        "space-runtime.service",
    ])
    .await;
    // Each automatic restart uses template defaults and a fresh startup probe.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    while property("MainPID").await == pid || property("NotifyAccess").await != "none" {
        assert!(
            tokio::time::Instant::now() < deadline,
            "automatic restart did not become ready"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready(&service(), 5).await.unwrap());
    assert_ne!(property("MainPID").await, pid);
    assert_eq!(property("NotifyAccess").await, "none");
    assert_eq!(
        std::fs::read_to_string("/run/runtime-env").unwrap(),
        "template"
    );
    assert!(
        std::fs::read_to_string("/run/runtime-probes")
            .unwrap()
            .len()
            > probes.len()
    );

    reset().await;
    declaration(&["/usr/bin/yes", "runtime output"], None);
    boot().await;
    assert!(ready(&service(), 5).await.unwrap());
    let observer = std::fs::File::open("/run/space-runtime/output.log").unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!((256 * 1024..=512 * 1024).contains(&observer.metadata().unwrap().len()));
    let main_pid: i32 = property("MainPID").await.parse().unwrap();
    let cgroup = property("ControlGroup").await;
    let pids = std::fs::read_to_string(format!("/sys/fs/cgroup{cgroup}/cgroup.procs")).unwrap();
    let relay: i32 = pids
        .lines()
        .map(|pid| pid.parse::<i32>().unwrap())
        .find(|pid| *pid != main_pid)
        .unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(relay),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(property("ExecMainStatus").await, "13");
    assert_eq!(property("NRestarts").await, "0");
    drop(observer);

    reset().await;
    declaration(&["/bin/sh", "-c", "echo crash-tail; exit 1"], None);
    boot().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while property("ActiveState").await != "failed" {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for path in ["output.log", "output.previous.log"] {
        assert!(
            std::fs::read_to_string(format!("/run/space-runtime/{path}"))
                .unwrap()
                .contains("crash-tail")
        );
    }

    reset().await;
    declaration(&["/nonexistent-runtime-app"], None);
    boot().await;
    assert_eq!(
        ready(&service(), 20).await.unwrap_err().code(),
        tonic::Code::FailedPrecondition
    );
    assert_eq!(property("NRestarts").await, "5");
    assert_eq!(
        ready(&service(), 2).await.unwrap_err().code(),
        tonic::Code::FailedPrecondition
    );
    assert_eq!(property("NRestarts").await, "5");
    reset().await;
}
