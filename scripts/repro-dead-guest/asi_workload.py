#!/usr/bin/env python3
"""ASI-like busy workload over create -> suspend -> restore cycles (phase 2, experiment 1).

One process per sandbox (examples/first_sandbox.py's request deadline uses
SIGALRM, which only works on the main thread):

  asi_workload.py run LABEL --out DIR --plan same,cross,cross --warm 600 --busy 3600 \
      [--memory-mib 2048 --disk-mib 4096 --mem-start-mb 200 --mem-final-mb 1500 --ramp 14400]
  asi_workload.py run LABEL --out DIR --attach --plan cross ...   # continue an existing sandbox

Each sandbox emulates an agent worker:
  * at every connect (after create and after each restore) it backfills CLI
    wrapper scripts with files:write and makes sure two long-lived processes
    run (an HTTP server, a log writer) plus `memhog`, a process that grows its
    resident memory toward /tmp/memtarget;
  * every 5-20 s one short command (shell, python, a VCS-like tree hash,
    a wrapper, curl to the local server, workspace writes);
  * every 60 s a telemetry command (free, loadavg, memhog RSS; dmesg tail every
    5th); every 2-3 min a 1-6 MB upload and a verified download.

Restores are serialized cluster-wide by DIR/restore.lock. Same-node or
cross-node placement is forced by cordoning the other sandbox worker during an
explicit :wake, after the suspend snapshot is portable (the smoke suite's
RESTORE_PORTABLE_SQL); the landed node is read from the sandbox pod.

Every operation is one JSON line in DIR/<label>.ops.jsonl; lifecycle steps go
to DIR/<label>.cycles.jsonl. Three consecutive failures open an incident:
capture.sh runs once, the workload keeps probing, and the incident closes after
three consecutive successes. An incident still open when its phase ends is
observed for at least 30 more minutes; if it is still open then, the sandbox is
left running for inspection and the process stops cycling it.
"""

import argparse
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import random
import subprocess
import sys
import time
import uuid

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import driver  # noqa: E402
fs = driver.fs

KCTX = ["kubectl", "--context", "kind-space-local", "-n", "space"]
WORKERS = ("space-local-worker", "space-local-worker2")
WORK = "/home/user/work"
BIN = "/home/user/.local/bin"

MEMHOG = r'''import os, random, time
chunks = []
tick = 0
while True:
    try:
        target = int(open("/tmp/memtarget").read().strip())
    except Exception:
        target = 0
    have = len(chunks) * 16
    if have + 16 <= target:
        block = bytearray(16 << 20)
        for i in range(0, len(block), 4096):
            block[i] = 1
        chunks.append(block)
        continue
    if have > target + 16:
        chunks.pop()
        continue
    tick += 1
    if chunks and tick % 5 == 0:  # keep part of the set hot
        block = random.choice(chunks)
        for i in range(0, len(block), 4096):
            block[i] = (block[i] + 1) & 0xFF
    with open("/tmp/memhog.mb", "w") as out:
        out.write(f"{have}\n")
    time.sleep(2)
'''

WRAPPERS = {
    "asi-ls": "#!/bin/sh\nexec ls -la \"${1:-.}\"\n",
    "asi-grep": "#!/bin/sh\nexec grep -rn \"${1:-x}\" \"${2:-.}\" 2>/dev/null | head -50\n",
    "asi-py": "#!/usr/bin/env python3\nimport sys, json, platform\nprint(json.dumps({'py': platform.python_version(), 'argv': sys.argv[1:]}))\n",
    "asi-hash": "#!/bin/sh\nfind \"${1:-.}\" -maxdepth 2 -type f 2>/dev/null | sort | head -200 | xargs -r sha1sum | sha1sum\n",
    "asi-fetch": "#!/bin/sh\nexec curl -s -o /dev/null -w '%{http_code} %{size_download}\\n' \"http://127.0.0.1:8000/${1:-}\"\n",
    "asi-env": "#!/bin/sh\nenv | sort | head -40 | wc -l\n",
    "asi-tree": "#!/bin/sh\ncd \"${1:-.}\" && find . -maxdepth 3 | sort | head -300 | wc -l\n",
    "asi-note": "#!/bin/sh\nmkdir -p \"$HOME/work/notes\" && date -u >> \"$HOME/work/notes/log.md\" && tail -n 3 \"$HOME/work/notes/log.md\"\n",
}

# Long-lived processes, started from a script file so `pgrep -f` never matches
# the launching shell's own command line.
LAUNCHER = f"""#!/bin/sh
pgrep -f 'python3 -m http.server 8000' >/dev/null || {{ setsid nohup python3 -m http.server 8000 --directory {WORK} </dev/null >/dev/null 2>&1 & }}
pgrep -f '{BIN}/asi-logwriter' >/dev/null || {{ setsid nohup {BIN}/asi-logwriter </dev/null >/dev/null 2>&1 & }}
pgrep -f 'python3 /home/user/.local/memhog.py' >/dev/null || {{ setsid nohup python3 /home/user/.local/memhog.py </dev/null >/dev/null 2>&1 & }}
sleep 1
pgrep -af 'http.server|asi-logwriter|memhog' | cut -c1-80
"""
WRAPPERS["asi-logwriter"] = f"#!/bin/sh\nwhile :; do date -u >> {WORK}/bg.log; sleep 2; done\n"

SHORT_COMMANDS = [
    ("sh", f"ls -la {WORK} | head -20; df -h / | tail -1"),
    ("python", "python3 -c \"import hashlib,os;print(hashlib.sha256(os.urandom(1<<20)).hexdigest())\""),
    ("python_json", "python3 -c \"import json,sys;print(len(json.dumps([{'i':i,'s':str(i)*20} for i in range(20000)])))\""),
    ("vcs_emul", f"cd {WORK} && find . -type f | sort | head -200 | xargs -r sha1sum | sha1sum"),
    ("wrapper", None),
    ("curl", "curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8000/"),
    ("write", f"dd if=/dev/urandom of={WORK}/scratch-$$ bs=256k count=8 2>/dev/null; "
              f"ls -t {WORK}/scratch-* | tail -n +11 | xargs -r rm -f; ls {WORK} | wc -l"),
    ("ps", "ps -eo pid,rss,etimes,args --sort=-rss | head -8"),
]


def now():
    return time.time()


def iso(t=None):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(t if t is not None else now()))


class Run:
    def __init__(self, args):
        self.args = args
        self.out = Path(args.out)
        self.out.mkdir(parents=True, exist_ok=True)
        self.label = args.label
        self.cfg = driver.config()
        self.state_path = self.out / f"{self.label}.json"
        self.state = json.loads(self.state_path.read_text()) if self.state_path.exists() else {}
        self.phase = "init"
        self.cycle = 0
        self.connected_at = now()
        self.fail_streak = 0
        self.ok_streak = 0
        self.incident = None
        self.dead = False
        self.started = now()
        self.token_at = now()

    def refresh_token(self):
        """Re-login every 30 min so a multi-hour run never depends on one session's lifetime."""
        if now() - self.token_at < 1800:
            return
        try:
            request = fs.Request(self.cfg["api_url"] + "/auth/v1/sessions",
                                 data=json.dumps({"email": "root@local", "password": os.environ.get(
                                     "SPACE_CONSOLE_PASSWORD", "password")}).encode(),
                                 headers={"Content-Type": "application/json"}, method="POST")
            with fs.deadline(30):
                with fs.HTTP.open(request, timeout=30) as response:
                    self.cfg["token"] = json.load(response)["token"]
            self.token_at = now()
        except Exception as error:  # noqa: BLE001 - keep the old token, retry next tick
            self.record("token_refresh", ok=False, err=str(error)[:200])

    # ---- recording ---------------------------------------------------
    def save(self):
        self.state_path.write_text(json.dumps(self.state, indent=2))

    def record(self, name, **fields):
        line = {"ts": iso(), "label": self.label, "sbx": self.state.get("sandbox_id"),
                "phase": self.phase, "cycle": self.cycle,
                "since_connect_s": round(now() - self.connected_at, 1), "op": name}
        line.update(fields)
        with open(self.out / f"{self.label}.ops.jsonl", "a") as out:
            out.write(json.dumps(line) + "\n")
        return line

    def lifecycle(self, step, **fields):
        line = {"ts": iso(), "label": self.label, "sbx": self.state.get("sandbox_id"),
                "cycle": self.cycle, "step": step}
        line.update(fields)
        with open(self.out / f"{self.label}.cycles.jsonl", "a") as out:
            out.write(json.dumps(line) + "\n")
        print(json.dumps(line), flush=True)

    def outcome(self, ok, name):
        if ok:
            self.ok_streak += 1
            self.fail_streak = 0
            if self.incident and self.ok_streak >= 3:
                self.incident["recovered_at"] = iso()
                self.incident["duration_s"] = round(now() - self.incident["t0"], 1)
                self.lifecycle("incident_recovered", **{k: v for k, v in self.incident.items() if k != "t0"})
                self.incident = None
        else:
            self.fail_streak += 1
            self.ok_streak = 0
            if not self.incident and self.fail_streak >= 3:
                self.open_incident(name)

    def open_incident(self, name):
        stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
        target = self.out / "incidents" / f"{self.label}-{stamp}"
        self.incident = {"t0": now(), "opened_at": iso(), "first_failed_op": name, "dir": str(target),
                         "since_connect_s": round(now() - self.connected_at, 1), "phase": self.phase}
        self.lifecycle("incident_opened", **{k: v for k, v in self.incident.items() if k != "t0"})
        target.mkdir(parents=True, exist_ok=True)
        subprocess.Popen([str(HERE / "capture.sh"), self.state["sandbox_id"], str(target)],
                         stdout=open(target / "capture.out", "w"), stderr=subprocess.STDOUT,
                         start_new_session=True)

    # ---- guest operations ---------------------------------------------
    def command(self, kind, script, timeout="60s", user=None):
        start = now()
        try:
            code, out = driver.run(self.cfg, self.state["sandbox_id"], ["sh", "-c", script],
                                   user=user, timeout=timeout, stream_timeout=120)
            ok = code == 0
            self.record("cmd", kind=kind, ok=ok, exit=code, ms=round((now() - start) * 1e3, 1),
                        out=out[-300:] if kind in {"telemetry", "connect"} or not ok else None)
            self.outcome(ok, f"cmd:{kind}")
            return ok, out
        except Exception as error:  # noqa: BLE001 - every failure is data
            body = getattr(error, "read", lambda: b"")()
            message = (str(error) + " " + body.decode(errors="replace"))[:400]
            self.record("cmd", kind=kind, ok=False, ms=round((now() - start) * 1e3, 1), err=message)
            self.outcome(False, f"cmd:{kind}")
            return False, ""

    def write_file(self, path, data, kind="file_write"):
        start = now()
        try:
            fs.api(self.cfg, "POST", f"/v1/sandboxes/{self.state['sandbox_id']}/files:write",
                   body={"path": path, "data_base64": base64.b64encode(data).decode()},
                   key=f"asi-w-{uuid.uuid4().hex}", timeout=120)
            self.record("file", kind=kind, ok=True, bytes=len(data), ms=round((now() - start) * 1e3, 1))
            self.outcome(True, kind)
            return True
        except Exception as error:  # noqa: BLE001
            body = getattr(error, "read", lambda: b"")()
            self.record("file", kind=kind, ok=False, bytes=len(data), ms=round((now() - start) * 1e3, 1),
                        err=(str(error) + " " + body.decode(errors="replace"))[:400])
            self.outcome(False, kind)
            return False

    def read_file(self, path, length):
        start = now()
        request = fs.Request(
            self.cfg["api_url"] + f"/v1/sandboxes/{self.state['sandbox_id']}/files:read",
            data=json.dumps({"path": path, "offset": 0, "length": length}).encode(),
            headers={"Authorization": f"Bearer {self.cfg['token']}", "Content-Type": "application/json"},
            method="POST")
        try:
            with fs.deadline(120):
                with fs.HTTP.open(request, timeout=120) as response:
                    data = response.read()
            return data, round((now() - start) * 1e3, 1), None
        except Exception as error:  # noqa: BLE001
            body = getattr(error, "read", lambda: b"")()
            return None, round((now() - start) * 1e3, 1), (str(error) + " " + body.decode(errors="replace"))[:400]

    def file_roundtrip(self):
        size = random.randint(1, 6) << 20
        data = os.urandom(size)
        name = f"{WORK}/upload-{uuid.uuid4().hex[:8]}.bin"
        if not self.write_file(name, data, kind="upload"):
            return
        got, ms, err = self.read_file(name, size)
        ok = got is not None and hashlib.sha256(got).digest() == hashlib.sha256(data).digest()
        self.record("file", kind="download", ok=ok, bytes=size, ms=ms, err=err)
        self.outcome(ok, "download")
        self.command("rotate_uploads", f"ls -t {WORK}/upload-*.bin | tail -n +5 | xargs -r rm -f")

    def mem_target(self):
        a = self.args
        frac = min(1.0, (now() - self.started) / max(1, a.ramp))
        return int(a.mem_start_mb + (a.mem_final_mb - a.mem_start_mb) * frac)

    def connect(self):
        """What an ASI worker does on (re)connect: backfill wrappers, ensure background processes."""
        self.connected_at = now()
        self.command("mkdirs", f"mkdir -p {WORK} {BIN}")
        for name, body in WRAPPERS.items():
            self.write_file(f"{BIN}/{name}", body.encode(), kind="backfill")
        self.write_file("/home/user/.local/memhog.py", MEMHOG.encode(), kind="backfill")
        self.write_file("/home/user/.local/asi-bg.sh", LAUNCHER.encode(), kind="backfill")
        script = (f"chmod 755 {BIN}/* /home/user/.local/asi-bg.sh; echo {self.mem_target()} > /tmp/memtarget; "
                  "/home/user/.local/asi-bg.sh; free -m | sed -n 2p")
        self.command("connect", script)

    def busy(self, seconds, label):
        self.phase = label
        end = now() + seconds
        next_cmd = now()
        next_tel = now()
        next_file = now() + random.uniform(30, 120)
        next_mem = now() + 300
        tel_count = 0
        while True:
            self.refresh_token()
            t = now()
            if t >= end:
                if not self.incident:
                    return True
                # keep observing an open incident for >= 30 min after it opened
                if t >= max(end, self.incident["t0"] + 1800):
                    self.dead = True
                    self.lifecycle("incident_unrecovered", opened_at=self.incident["opened_at"],
                                   observed_s=round(t - self.incident["t0"], 1))
                    return False
            if t >= next_mem:
                self.command("mem_target", f"echo {self.mem_target()} > /tmp/memtarget; cat /tmp/memhog.mb 2>/dev/null")
                next_mem = t + 300
            if t >= next_tel:
                tel_count += 1
                extra = "; dmesg 2>/dev/null | tail -20" if tel_count % 5 == 0 else ""
                self.command("telemetry", "free -m | sed -n 2p; cat /proc/loadavg; "
                                          "cat /tmp/memhog.mb 2>/dev/null; pgrep -c -f 'http.serve[r]|asi-logwrite[r]|memho[g].py'"
                                          + extra, user="root")
                next_tel = t + 60
            if t >= next_file:
                self.file_roundtrip()
                next_file = t + random.uniform(120, 180)
            if t >= next_cmd:
                kind, script = random.choice(SHORT_COMMANDS)
                if kind == "wrapper":
                    script = f"PATH={BIN}:$PATH; " + random.choice([
                        f"asi-ls {WORK}", f"asi-grep UTC {WORK}/notes", "asi-py a b", f"asi-hash {WORK}",
                        "asi-fetch", "asi-env", f"asi-tree {WORK}", "asi-note"])
                self.command(kind, script)
                # while an incident is open, probe every 15 s
                next_cmd = now() + (15 if self.incident else random.uniform(5, 20))
            time.sleep(max(0.2, min(next_cmd, next_tel, next_file, next_mem, end) - now()))

    # ---- lifecycle ------------------------------------------------------
    def pod_node(self):
        uid = self.state["sandbox_id"].removeprefix("sbx_")
        out = subprocess.run(KCTX + ["get", "pods", "-l", f"space.pplx.net/sandbox-id={uid}", "-o",
                                     "jsonpath={range .items[*]}{.spec.nodeName} {.status.phase}{\"\\n\"}{end}"],
                             capture_output=True, text=True, timeout=60).stdout.split("\n")
        running = [line.split()[0] for line in out if line.strip().endswith("Running")]
        return running[0] if running else (out[0].split()[0] if out and out[0].strip() else None)

    def portable(self):
        uid = self.state["sandbox_id"].removeprefix("sbx_")
        sql = (
            "SELECT EXISTS (SELECT 1 FROM sandboxes s WHERE s.id = '%s'::uuid "
            "AND s.lifecycle IN ('suspended','restoring') AND EXISTS (SELECT 1 FROM snapshots sn "
            "JOIN snapshot_storage_locations ssl ON ssl.snapshot_id = sn.id WHERE sn.sandbox_id = s.id "
            "AND sn.type = 'full' AND sn.state = 'ready' AND (s.placement_changed_at IS NULL "
            "OR sn.timestamp >= s.placement_changed_at)));" % uid)
        out = subprocess.run(KCTX + ["exec", "-i", "space-postgres-0", "--", "psql", "-U", "spaceuser",
                                     "-d", "spacedb", "-tA", "-c", sql],
                             capture_output=True, text=True, timeout=60).stdout.strip()
        return out == "t"

    def op(self, action):
        start = now()
        result = fs.api(self.cfg, "POST", f"/v1/sandboxes/{self.state['sandbox_id']}:{action}", body={},
                        key=f"asi-{action}-{uuid.uuid4().hex}")
        deadline = now() + 900
        while True:
            op = fs.api(self.cfg, "GET", f"/v1/operations/{result['operation_id']}")
            if op["state"] in {"succeeded", "failed", "cancelled"} or now() > deadline:
                break
            time.sleep(2)
        return op["state"], round(now() - start, 1), op.get("terminal_error")

    def create(self):
        a = self.args
        body = {"template": self.cfg["template"], "runtime": "fc", "name": f"asi-{self.label}",
                "metadata": {"repro": "phase2-exp1", "label": self.label},
                "resources": {"vcpu": 2, "memory_mib": a.memory_mib, "disk_mib": a.disk_mib},
                "auto_lifecycle": driver.policy("8h")}
        result = fs.api(self.cfg, "POST", "/v1/sandboxes", body=body, key=f"asi-{self.label}-{uuid.uuid4().hex}")
        self.state.update(sandbox_id=result["sandbox_id"], created_at=iso())
        self.save()
        fs.wait_for_state(self.cfg, f"/v1/operations/{result['operation_id']}", "succeeded")
        fs.wait_for_state(self.cfg, f"/v1/sandboxes/{result['sandbox_id']}", "running")
        self.state["node"] = self.pod_node()
        self.save()
        self.lifecycle("created", node=self.state["node"])

    def suspend_restore(self, kind):
        source = self.pod_node() or self.state.get("node")
        state, secs, err = self.op("suspend")
        self.lifecycle("suspend", result=state, secs=secs, error=err, node=source)
        if state != "succeeded":
            return False
        t0 = now()
        while not self.portable() and now() - t0 < 600:
            time.sleep(3)
        portable_s = round(now() - t0, 1)
        target = source if kind == "same" else next(n for n in WORKERS if n != source)
        other = next(n for n in WORKERS if n != target)
        with open(self.out / "restore.lock", "w") as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            waited = round(now() - t0 - portable_s, 1)
            subprocess.run(KCTX[:3] + ["cordon", other], capture_output=True, timeout=60)
            try:
                state, secs, err = self.op("wake")
            finally:
                subprocess.run(KCTX[:3] + ["uncordon", other], capture_output=True, timeout=60)
        landed = self.pod_node()
        self.state["node"] = landed
        self.save()
        self.lifecycle("restore", kind=kind, result=state, secs=secs, error=err, source=source,
                       target=target, landed=landed, portable_wait_s=portable_s, lock_wait_s=waited,
                       cross_node=landed != source)
        return state == "succeeded"


def run(args):
    r = Run(args)
    if args.attach:
        r.started = now() - args.elapsed
    else:
        r.create()
    r.phase = "warm"
    r.connect()
    if not r.busy(args.warm, "warm"):
        return
    for index, kind in enumerate([k for k in args.plan.split(",") if k]):
        r.cycle = index + 1
        if not r.suspend_restore(kind):
            r.lifecycle("cycle_aborted")
            return
        r.phase = f"post-restore-{r.cycle}"
        r.connect()
        if not r.busy(args.busy, f"post-restore-{r.cycle}"):
            return
    r.lifecycle("plan_done")


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("run")
    p.add_argument("label")
    p.add_argument("--out", required=True)
    p.add_argument("--attach", action="store_true")
    p.add_argument("--elapsed", type=float, default=0, help="with --attach: seconds already ramped")
    p.add_argument("--plan", default="same,cross,same")
    p.add_argument("--warm", type=float, default=600)
    p.add_argument("--busy", type=float, default=3600)
    p.add_argument("--memory-mib", type=int, default=2048)
    p.add_argument("--disk-mib", type=int, default=4096)
    p.add_argument("--mem-start-mb", type=int, default=200)
    p.add_argument("--mem-final-mb", type=int, default=1500)
    p.add_argument("--ramp", type=float, default=4 * 3600)
    args = parser.parse_args()
    run(args)


if __name__ == "__main__":
    main()
