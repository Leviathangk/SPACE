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
Readiness is a startup gate, not an ongoing health check.

PID1 restarts failures after two seconds, with five starts per 30-second window.
A clean exit stays stopped. The app must remain in the foreground; descendants are
stopped with its unit. Startup is not coupled to spaced restarts. The app remains
systemd's MainPID, preserving signal-exit semantics.

Output is a bounded, wrapping plain-text file at `/run/space-runtime/output.log`
(up to 1 MiB). File readers cannot backpressure the app. A log-relay failure can
produce SIGPIPE or EPIPE; SIGPIPE is a clean exit under `Restart=on-failure`.
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
Legacy startup definitions or enablement links are rejected on capable outputs;
rebuild those lineages from recipes. Old artifacts retain their existing behavior.

Deploy builder masking before publishing runtime-bearing parents. Hold new ECW
build admission and drain existing jobs while switching the translator and builder,
then publish the new seed, code-runtime, and child chain. Test the aliases used by
callers before reopening admission. An old translator selecting a new capable base
would otherwise write a legacy unit and fail finalization. Old hosts need no new
RPC support, though their readiness timeout may be too short for slow cold starts.

Before deployment, validate cold boot, base snapshots, pool adoption, stop/start,
pause/resume, suspend/restore and agent/node restarts on Firecracker. The disposable
systemd test covers guest process supervision, not VM or network behavior.
