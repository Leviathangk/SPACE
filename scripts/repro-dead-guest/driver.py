#!/usr/bin/env python3
"""Drive the dead-guest auto-suspend reproduction against a local Kind stack.

Standard library only; reuses the request helpers from examples/first_sandbox.py.

  driver.py login
  driver.py create LABEL --idle 90s          # native sandbox, AccessIdle -> suspend
  driver.py exec SBX [--user root] -- CMD...    # run a command, print output
  driver.py stoploop SBX --stop-min S --stop-jitter J --cont C
  driver.py get SBX                           # management read (no guest access)

All guest-touching calls (exec, stoploop) count as access/command activity and
reset the idle clock; the observation phase must only use `get` and host logs.
"""

import argparse
import base64
import json
import os
from pathlib import Path
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "examples"))
import first_sandbox as fs  # noqa: E402

STATE_DIR = Path(os.environ.get("REPRO_STATE_DIR", Path.home() / "repro-logs" / "runs"))
TOKEN_FILE = Path(os.environ.get("REPRO_TOKEN_FILE", Path.home() / "repro-logs" / ".token"))


def config():
    os.environ.setdefault("SPACE_API_TOKEN", TOKEN_FILE.read_text().strip() if TOKEN_FILE.exists() else "")
    return fs.configuration()


def login(_args):
    password = os.environ.get("SPACE_CONSOLE_PASSWORD", "password")
    request = fs.Request(
        os.environ.get("SPACE_API_URL", "http://127.0.0.1:8080") + "/auth/v1/sessions",
        data=json.dumps({"email": "root@local", "password": password}).encode(),
        headers={"Content-Type": "application/json"}, method="POST")
    with fs.HTTP.open(request, timeout=15) as response:
        token = json.load(response)["token"]
    TOKEN_FILE.write_text(token)
    TOKEN_FILE.chmod(0o600)
    print("token saved")


def policy(idle):
    return {
        "wake": {"on": ["access", "command"]},
        "condition_sets": {"access_idle": {"kind": "inactivity", "activity_source": "access"}},
        "stages": [{"condition_set": "access_idle", "at": idle, "action": "suspend"}],
    }


def create(args):
    cfg = config()
    body = {"template": cfg["template"], "runtime": "fc", "name": f"repro-{args.label}",
            "metadata": {"repro": args.label}, "auto_lifecycle": policy(args.idle)}
    result = fs.api(cfg, "POST", "/v1/sandboxes", body=body, key=f"repro-{args.label}-{uuid.uuid4().hex}")
    sbx, op = result["sandbox_id"], result["operation_id"]
    print(f"sandbox_id={sbx} operation_id={op}", flush=True)
    fs.wait_for_state(cfg, f"/v1/operations/{op}", "succeeded")
    fs.wait_for_state(cfg, f"/v1/sandboxes/{sbx}", "running")
    fs.api(cfg, "GET", f"/v1/sandboxes/{sbx}/processes?limit=1", timeout=180)
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    (STATE_DIR / f"{args.label}.json").write_text(json.dumps(
        {"label": args.label, "sandbox_id": sbx, "operation_id": op, "idle": args.idle,
         "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}, indent=2))
    print(sbx)


def run(cfg, sbx, command, user=None, timeout="60s", stream_timeout=90):
    body = {"command": command, "timeout": timeout}
    if user:
        body["user"] = user
    result = fs.api(cfg, "POST", f"/v1/sandboxes/{sbx}/commands", body=body,
                    key=f"repro-cmd-{uuid.uuid4().hex}")
    attach = result["attach"]
    url = attach["events_url"]
    if url.startswith("/"):
        url = cfg["data_plane_url"] + url
    request = fs.Request(url, headers={"Authorization": f"Bearer {attach['token']}",
                                       "Accept": "text/event-stream"})
    out, code = bytearray(), None

    class Sink:
        def write(self, _):
            pass

        def flush(self):
            pass

    with fs.deadline(stream_timeout):
        with fs.HTTP.open(request, timeout=stream_timeout) as response:
            for frame in fs.event_frames(response, Sink()):
                kind = frame.get("type")
                if kind in {"stdout", "stderr"}:
                    out += base64.b64decode(frame["data_base64"])
                elif kind == "process_exited":
                    code = frame.get("exit_code")
                elif kind in {"error", "resync_required"}:
                    raise RuntimeError(f"stream {kind}: {frame}")
    return code, out.decode(errors="replace")


def exec_cmd(args):
    code, out = run(config(), args.sandbox, args.command, user=args.user)
    sys.stdout.write(out)
    print(f"[exit {code}]")


# The loop runs as root, detached with setsid so it survives the launching
# command and is not in the agent's process group. It never signals itself:
# only spaced.service's MainPID is stopped. Each STOP/CONT is logged with a
# UTC timestamp to /var/log/repro-stoploop.log and mirrored to /dev/kmsg.
LOOP = r'''#!/bin/sh
LOG=/var/log/repro-stoploop.log
STOP_MIN=$1; STOP_JITTER=$2; CONT=$3
log() { m="$(date -u +%Y-%m-%dT%H:%M:%S.%NZ) $*"; echo "$m" >> $LOG; echo "repro-stoploop: $m" > /dev/kmsg 2>/dev/null; }
pid=$(systemctl show -p MainPID --value spaced.service)
log "start pid=$pid self=$$ stop_min=$STOP_MIN stop_jitter=$STOP_JITTER cont=$CONT"
trap 'kill -CONT $pid; log "exit CONT pid=$pid"; exit 0' TERM INT
# Let the launching command exit and its data-plane stream close first, so no
# open ingress connection defers the auto-lifecycle transition.
sleep 5
while :; do
  cur=$(systemctl show -p MainPID --value spaced.service 2>/dev/null)
  [ -n "$cur" ] && [ "$cur" != 0 ] && [ "$cur" != "$pid" ] && { log "mainpid changed $pid -> $cur"; pid=$cur; }
  s=$(( STOP_MIN + $(od -An -N2 -tu2 /dev/urandom) % (STOP_JITTER + 1) ))
  kill -STOP $pid && log "STOP pid=$pid for=${s}s"
  sleep $s
  kill -CONT $pid && log "CONT pid=$pid for=${CONT}s"
  sleep $CONT
done
'''


def stoploop(args):
    cfg = config()
    script = base64.b64encode(LOOP.encode()).decode()
    launch = (
        f"echo {script} | base64 -d > /usr/local/bin/repro-stoploop && chmod 755 /usr/local/bin/repro-stoploop && "
        f"setsid nohup /usr/local/bin/repro-stoploop {args.stop_min} {args.stop_jitter} {args.cont} "
        "</dev/null >/dev/null 2>&1 & sleep 1; "
        "echo spaced_mainpid=$(systemctl show -p MainPID --value spaced.service); "
        "pgrep -af repro-stoploop; cat /var/log/repro-stoploop.log"
    )
    code, out = run(cfg, args.sandbox, ["sh", "-c", launch], user="root")
    sys.stdout.write(out)
    print(f"[exit {code}]")


def stoplog(args):
    """Stop the loop (its TERM trap CONTs the agent) and print its log.

    The command is delivered by the agent itself, so it runs at the next CONT
    window; the stream timeout must exceed the longest STOP period. This is
    guest activity and resets the idle clock: use it only after the window.
    """
    # The bracket keeps pkill from matching this command's own `sh -c` line.
    script = ("pkill -TERM -f '^/bin/sh /usr/local/bin/[r]epro-stoploop'; sleep 1; "
              "pgrep -af '[r]epro-stoploop'; cat /var/log/repro-stoploop.log" if args.stop
              else "cat /var/log/repro-stoploop.log")
    # A stopped agent makes command creation fail (503); retry until a CONT window.
    deadline = time.monotonic() + args.wait
    while True:
        try:
            code, out = run(config(), args.sandbox, ["sh", "-c", script], user="root",
                            timeout="5m", stream_timeout=300)
            break
        except Exception as error:  # noqa: BLE001 - retried until the deadline
            if time.monotonic() > deadline:
                raise
            print(f"{time.strftime('%H:%M:%S', time.gmtime())} retry: {str(error)[:120]}",
                  file=sys.stderr, flush=True)
            time.sleep(2)
    sys.stdout.write(out)
    print(f"[exit {code}]")


def wake(args):
    cfg = config()
    result = fs.api(cfg, "POST", f"/v1/sandboxes/{args.sandbox}:wake", body={},
                    key=f"repro-wake-{uuid.uuid4().hex}")
    fs.wait_for_state(cfg, f"/v1/operations/{result['operation_id']}", "succeeded")
    print(fs.api(cfg, "GET", f"/v1/sandboxes/{args.sandbox}")["state"])


def suspend(args):
    cfg = config()
    result = fs.api(cfg, "POST", f"/v1/sandboxes/{args.sandbox}:suspend", body={},
                    key=f"repro-suspend-{uuid.uuid4().hex}")
    fs.wait_for_state(cfg, f"/v1/operations/{result['operation_id']}", "succeeded")
    print(fs.api(cfg, "GET", f"/v1/sandboxes/{args.sandbox}")["state"])


def delete(args):
    cfg = config()
    for sbx in args.sandboxes:
        try:
            result = fs.api(cfg, "DELETE", f"/v1/sandboxes/{sbx}", key=f"repro-del-{uuid.uuid4().hex}")
            print(sbx, result.get("operation_id"))
        except Exception as error:  # noqa: BLE001 - report and continue with the rest
            print(sbx, "error:", str(error)[:200])


def probe(args):
    """One Process.Start reachability sample per sandbox: ok/error and latency."""
    cfg = config()
    for sbx in args.sandboxes:
        start = time.monotonic()
        try:
            code, out = run(cfg, sbx, ["sh", "-c", "echo ok"], timeout="20s", stream_timeout=30)
            result = {"ok": code == 0 and out.strip() == "ok", "exit": code}
        except Exception as error:  # noqa: BLE001 - recorded as a failed sample
            body = getattr(error, "read", lambda: b"")()
            result = {"ok": False, "error": (str(error) + " " + body.decode(errors="replace"))[:300]}
        result.update(ts=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), sbx=sbx,
                      ms=round((time.monotonic() - start) * 1e3, 1))
        print(json.dumps(result), flush=True)


def get(args):
    result = fs.api(config(), "GET", f"/v1/sandboxes/{args.sandbox}")
    print(json.dumps(result if args.full else {k: result.get(k) for k in ("id", "state", "updated_at")}))


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    sub.add_parser("login").set_defaults(fn=login)
    p = sub.add_parser("create")
    p.add_argument("label")
    p.add_argument("--idle", default="90s")
    p.set_defaults(fn=create)
    p = sub.add_parser("exec")
    p.add_argument("sandbox")
    p.add_argument("--user")
    p.set_defaults(fn=exec_cmd)
    p = sub.add_parser("stoploop")
    p.add_argument("sandbox")
    p.add_argument("--stop-min", type=int, required=True)
    p.add_argument("--stop-jitter", type=int, required=True)
    p.add_argument("--cont", type=int, default=2)
    p.set_defaults(fn=stoploop)
    p = sub.add_parser("stoplog")
    p.add_argument("sandbox")
    p.add_argument("--stop", action="store_true")
    p.add_argument("--wait", type=int, default=6000)
    p.set_defaults(fn=stoplog)
    p = sub.add_parser("wake")
    p.add_argument("sandbox")
    p.set_defaults(fn=wake)
    p = sub.add_parser("suspend")
    p.add_argument("sandbox")
    p.set_defaults(fn=suspend)
    p = sub.add_parser("delete")
    p.add_argument("sandboxes", nargs="+")
    p.set_defaults(fn=delete)
    p = sub.add_parser("probe")
    p.add_argument("sandboxes", nargs="+")
    p.set_defaults(fn=probe)
    p = sub.add_parser("get")
    p.add_argument("sandbox")
    p.add_argument("--full", action="store_true")
    p.set_defaults(fn=get)
    argv = sys.argv[1:]
    command = []
    if "--" in argv:
        command = argv[argv.index("--") + 1:]
        argv = argv[:argv.index("--")]
    args = parser.parse_args(argv)
    args.command = command
    args.fn(args)


if __name__ == "__main__":
    main()
