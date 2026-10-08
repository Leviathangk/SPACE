# Native template startup

A template can declare one foreground application to start at boot. The declaration
lives at `/etc/space/runtime.json` in the rootfs and is covered by its content hash.
The platform owns `space-runtime.service`; recipes supply data, not unit text.

Append one `set_runtime` step to a native template-build request:

```json
{
  "set_runtime": {
    "schema_version": 1,
    "command": ["/usr/local/bin/app", "--port", "8080"],
    "user": "user",
    "working_directory": "/home/user",
    "env": {"APP_MODE": "template"},
    "readiness": {"type": "exec", "command": ["/usr/bin/curl", "-fsS", "http://127.0.0.1:8080/ready"]},
    "startup_timeout_secs": 60
  }
}
```

For TOML recipes, the equivalent final step is:

```toml
[[steps]]
type = "set-runtime"
[steps.declaration]
schema_version = 1
command = ["/usr/local/bin/app", "--port", "8080"]
user = "user"
working_directory = "/home/user"
startup_timeout_secs = 60
[steps.declaration.env]
APP_MODE = "template"
[steps.declaration.readiness]
type = "exec"
command = ["/usr/bin/curl", "-fsS", "http://127.0.0.1:8080/ready"]
```

Use `{"clear_runtime": {}}`, or `type = "clear-runtime"` in TOML, to disable
inherited startup. Omit both to inherit a FromTemplate parent's exact declaration.
OCI origins start without a declaration; OCI CMD and ENTRYPOINT remain ignored.
Scratch seed builds do not accept recipe steps: build a child of the seed to add
an application. A runtime step must be last, appear only once, and have no
`env_refs`. Template settings must be literal and reproducible.

The declaration is limited to 256 KiB. Commands are argv arrays, the working
directory must be absolute, and startup timeout is 1–60 seconds. `{"type":"launch"}`
readiness waits for successful exec only. An exec probe runs repeatedly until it
succeeds or the startup deadline expires. Application and probe use the same user,
working directory and template environment. Account defaults provide HOME, PATH,
USER, LOGNAME and SHELL; declaration values override them.

## Boot and snapshot behavior

The application uses template settings on every cold boot. Per-sandbox environment
variables, secrets and identity are **not** supplied to the application; they still
apply to commands launched later through process APIs. The app starts after the
agent, in `space-sessions.slice`, and survives agent restarts.

Guest boot readiness includes application startup. Base snapshot creation already
waits for guest readiness, so snapshots contain the ready app. Creates from a base
snapshot and pool adoption resume that app; they do not re-run its readiness probe.
Cold boots pay startup latency. A rootfs can publish successfully and later fail
boot or base snapshot creation if its app never becomes ready. Build-time readiness
validation is not part of this feature.

Do not bake per-sandbox secrets, unique tokens or identity into application startup.
Snapshots clone application memory, random state, files and open connections.
Applications must tolerate that cloning and network reconnection. Cold boot can
start the app before networking is usable; early outbound work needs retries.
Readiness is a startup gate, not an ongoing health check. A receipt in
`/run/space-runtime/boot-ready` records successful readiness, including absence of
an application. It survives spaced restarts and memory restore, so subsequent app
failure or tenant declaration edits cannot block resume. A cold boot clears it.

PID1 restarts failures after two seconds, with five starts per 30-second window.
A clean exit stays stopped. The app must remain in the foreground; descendants are
stopped with its unit. Startup is not coupled to spaced restarts. The app remains
systemd's MainPID, preserving signal-exit semantics.

Application and probe output goes to `/run/space-runtime/output.log` (512 KiB),
with the prior invocation in `output.previous.log` (512 KiB). Wrapping retains
at least 256 KiB of context; both files reuse their inodes so open readers cannot
pin rotated storage. Exit drains are bounded, so late output may still be lost.
Early launcher errors go to the rate-limited journal. File readers cannot
backpressure the app. A log-relay failure can produce SIGPIPE or EPIPE; SIGPIPE is a clean exit under `Restart=on-failure`.
Diagnostics are guest-local and disappear with guest cleanup. There is no new log
API or persisted failure-detail store.

## Build and rollout contracts

Builder VMs cold-boot with `systemd.mask=space-runtime.service`. Before user steps,
the builder verifies that mask and the installed guest backend, and records the
expected declaration. It reads back and validates exact bytes at finalization,
including inherited declarations. Editing the file through ordinary steps fails
finalization; use the native runtime step. Runtime-capable builds require a
successful filesystem flush and confirmed VM stop before rootfs copy.

Older protobuf readers drop unknown runtime step bodies; the builder rejects the
missing body instead of silently ignoring startup intent. Native runtime steps
require a rebuilt, capable parent, including its fixed unit and boot target.
Legacy-only output remains valid while old ECW translators drain. A declaration
combined with legacy startup definitions or enablement links is rejected: rebuild
those lineages from recipes. Resource-only legacy drop-ins remain for rolling
compatibility; they neither define nor enable applications.

Roll out without rewriting existing sandbox images:

1. Deploy host/builder support, including builder masking and the larger cold-start
   budget. New hosts use the unchanged readiness protocol with both guest generations.
2. Rebuild seed and code-runtime before cutting ECW over to native declarations.
   Old translators can still build legacy-only output from these capable bases.
3. Deploy the ECW translator across all API instances, then publish native application
   children and switch new creates to the rebuilt lineage. During the API rollout,
   avoid using native runtime-bearing parents through an old translator, which would
   produce rejected mixed startup. Old custom parents may require rebuilding.
4. Keep existing sandboxes and their artifacts. Pause/resume and suspend/restore retain
   the old guest. Stop/start uses the existing rootfs, or resolves its saved template
   hash if preparation is needed; never retarget it to the latest alias. Retain old
   template artifacts while sandboxes or snapshots still reference them. If both the
   artifact and saved hash are missing, start fails explicitly rather than selecting
   a replacement by name. Pool adoption matches the resolved content hash, so old
   pool members cannot satisfy creates pinned to the new build.

The node's default cold-start wait is 90 seconds, covering the declaration's
60-second startup cap plus boot time. Explicit caller deadlines still win. The API's
normal blocking budget is 135 seconds. Older hosts default to 30 seconds; upgrade
hosts before selecting new runtime templates. Pools inherit the node default.
Warm resume retains its shorter budget because it reads the boot receipt.

Before deployment, validate cold boot, base snapshots, pool adoption, stop/start,
pause/resume, suspend/restore and agent/node restarts on Firecracker. The disposable
systemd test covers guest process supervision, not VM or network behavior.

## Run the systemd acceptance harness

The example changes units and files; run only in a disposable Linux container with
private cgroups and no host mounts. On a Linux builder (or using the matching musl
cross target on macOS), build `spaced-svc` and the `runtime_systemd` example:

```sh
cargo build -p pplx-space-spaced-guest-svc --bin spaced-svc
cargo build -p pplx-space-spaced-guest-server --example runtime_systemd
```

Build an Ubuntu 26.04 fixture image with `systemd`, `systemd-sysv`, and `python3`
installed. Launch PID1 with `--privileged --cgroupns=private --tmpfs /run
--tmpfs /run/lock`, without host mounts. Copy the two binaries to
`/usr/local/bin/spaced-svc` and `/runtime_systemd`. Inside that disposable container:

```sh
touch /run/space-runtime-test-container
/usr/local/bin/spaced-svc --runtime-unit > /etc/systemd/system/space-runtime.service
printf '[Unit]\nDefaultDependencies=no\nWants=space-runtime.service\nBefore=space-runtime.service\n' > /etc/systemd/system/space.target
systemctl daemon-reload
/runtime_systemd
```

The marker is an accidental-execution guard, not isolation. Remove the container
and its task-owned image after testing. This harness is explicitly invoked, not
part of the workspace test suite; it covers real PID1 supervision and recovery,
not Firecracker boot, networking, pool adoption, or snapshot transfer.
