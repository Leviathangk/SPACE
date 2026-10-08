//! Run explicitly in a disposable systemd container with the native runtime
//! unit and built spaced-svc installed. The marker prevents accidental host use.
#![cfg(target_os = "linux")]

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
        SpacedGuestServiceConfig::default(),
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

#[tokio::test]
#[ignore = "requires disposable systemd container, seed unit and spaced-svc binary"]
async fn template_boot_lifecycle_under_pid1() {
    assert!(
        std::path::Path::new("/run/space-runtime-test-container").is_file(),
        "refusing to change host units"
    );
    reset().await;
    boot().await;
    assert!(ready(&service(), 5).await.unwrap());
    assert_eq!(property("MainPID").await, "0");

    reset().await;
    declaration(&["/usr/local/bin/spaced-svc", "--hold-cgroup"], None);
    boot().await;
    let guest = service();
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
    // Boot readiness is a latch, not a health monitor. A restarted agent must
    // reconstruct the result from the now-terminal unit rather than start it.
    assert!(ready(&guest, 2).await.unwrap());
    assert_eq!(
        ready(&service(), 2).await.unwrap_err().code(),
        tonic::Code::FailedPrecondition
    );

    reset().await;
    declaration(
        &[
            "/bin/sh",
            "-c",
            "printf '%s' \"$RUNTIME_TEST\" > /run/runtime-env; exec /bin/sleep infinity",
        ],
        Some("echo probe >> /run/runtime-probes; test -f /run/runtime-ready"),
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
    tokio::time::sleep(Duration::from_millis(150)).await;
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
    assert!(observer.metadata().unwrap().len() <= 1024 * 1024);
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
