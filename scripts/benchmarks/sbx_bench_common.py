#!/usr/bin/env python3
"""Shared helpers for SPACE sandbox benchmark scripts."""

from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import math
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Iterable, Sequence


UUID_RE = re.compile(
    r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
)
BOOT_MODE_RE = re.compile(r"^Boot mode:\s+(\S+)", re.MULTILINE)
DEFAULT_SSH_OPTIONS = [
    "StrictHostKeyChecking=no",
    "ConnectTimeout=10",
    "ControlMaster=no",
    "ControlPath=none",
]


def eprint(*args: object) -> None:
    print(*args, file=sys.stderr, flush=True)


def now_ms() -> int:
    return time.monotonic_ns() // 1_000_000


def timestamp_for_path() -> str:
    return time.strftime("%Y%m%dT%H%M%S")


def parse_csv(value: str) -> list[str]:
    return [item.strip() for item in value.split(",") if item.strip()]


def parse_int_csv(value: str, *, name: str) -> list[int]:
    out: list[int] = []
    for item in parse_csv(value):
        try:
            parsed = int(item)
        except ValueError as exc:
            raise SystemExit(f"ERROR: invalid {name} CSV item: {item}") from exc
        if parsed < 0:
            raise SystemExit(f"ERROR: {name} values must be >= 0: {item}")
        out.append(parsed)
    if not out:
        raise SystemExit(f"ERROR: {name} cannot be empty")
    return out


def percentile(sorted_values: Sequence[int], pct: int) -> int | None:
    if not sorted_values:
        return None
    idx = math.ceil(len(sorted_values) * pct / 100) - 1
    idx = max(0, min(idx, len(sorted_values) - 1))
    return sorted_values[idx]


def format_ms(value: int | float | None) -> str:
    if value is None:
        return "-"
    if isinstance(value, float):
        return f"{value:.1f}"
    return str(value)


def stats(values: Iterable[int]) -> dict[str, str | int]:
    ordered = sorted(values)
    if not ordered:
        return {
            "count": 0,
            "min": "-",
            "p50": "-",
            "p90": "-",
            "p95": "-",
            "p99": "-",
            "max": "-",
            "mean": "-",
        }
    return {
        "count": len(ordered),
        "min": ordered[0],
        "p50": percentile(ordered, 50),
        "p90": percentile(ordered, 90),
        "p95": percentile(ordered, 95),
        "p99": percentile(ordered, 99),
        "max": ordered[-1],
        "mean": f"{sum(ordered) / len(ordered):.1f}",
    }


def command_exists(path: str) -> bool:
    return os.access(path, os.X_OK) or shutil.which(path) is not None


def run_capture(cmd: Sequence[str], *, timeout: float | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        list(cmd),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=timeout,
    )


def extract_sandbox_id(text: str) -> str:
    match = UUID_RE.search(text)
    return match.group(0) if match else ""


def sanitize_error(text: str, limit: int = 180) -> str:
    one_line = " ".join(text.replace("\t", " ").split())
    return one_line[:limit] if one_line else "-"


def extract_boot_mode(text: str) -> str:
    match = BOOT_MODE_RE.search(text)
    return match.group(1) if match else ""


@dataclasses.dataclass(frozen=True)
class CommandResult:
    operation: str
    background: str | int
    burst: str | int
    repeat: str | int
    index: int
    ms: int
    rc: int
    sandbox_id: str
    error: str
    log_file: str


def choose_sudo_prefix(sbxmgr: str, use_sudo: str) -> list[str]:
    if use_sudo == "yes":
        return ["sudo"]
    if use_sudo == "no":
        return []
    if run_capture([sbxmgr, "list"]).returncode == 0:
        return []
    if run_capture(["sudo", "-n", sbxmgr, "list"]).returncode == 0:
        return ["sudo"]
    raise SystemExit(
        f"ERROR: cannot run {sbxmgr} directly or with passwordless sudo. "
        "Use --sudo or run as a user in the SPACE client groups."
    )


def checked_prereq(path: str, label: str) -> None:
    if not command_exists(path):
        raise SystemExit(f"ERROR: {label} not found or not executable: {path}")


def ensure_template(prefix: Sequence[str], tmplmgr: str, template: str) -> None:
    result = run_capture([*prefix, tmplmgr, "list"])
    if result.returncode != 0:
        raise SystemExit(f"ERROR: failed to list templates:\n{result.stdout}")
    if template not in result.stdout:
        raise SystemExit(f"ERROR: template '{template}' not found:\n{result.stdout}")


@dataclasses.dataclass
class SandboxCli:
    prefix: list[str]
    sbxmgr: str
    tmplmgr: str
    nodemgr: str
    template: str
    runtime: str
    boot_mode: str
    user: str = ""
    environment: str = ""
    content_hash: str = ""

    def create_cmd(self, *, autostart: bool = True, snapshots: bool = False) -> list[str]:
        cmd = [
            *self.prefix,
            self.sbxmgr,
            "create",
            "--template",
            self.template,
            "--boot-mode",
            self.boot_mode,
        ]
        if self.environment:
            cmd += ["--environment", self.environment]
        else:
            cmd += ["--runtime", self.runtime]
        if self.content_hash:
            cmd += ["--content-hash", self.content_hash]
        if self.user:
            cmd += ["--user", self.user]
        if autostart:
            cmd += ["--autostart", "--blocking"]
        if snapshots:
            cmd += ["--snapshots"]
        return cmd

    def id_cmd(self, operation: str, sandbox_id: str, ready_timeout_secs: str = "") -> list[str]:
        command = "start" if operation == "start_base_snap" else operation
        cmd = [*self.prefix, self.sbxmgr, command, sandbox_id]
        if operation == "start_base_snap":
            cmd += ["--from-warm", "--blocking"]
        elif operation in {"resume", "restore"}:
            cmd.append("--blocking")
            if ready_timeout_secs:
                cmd += ["--ready-timeout-secs", ready_timeout_secs]
        return cmd

    def delete(self, sandbox_id: str) -> None:
        result = run_capture([*self.prefix, self.sbxmgr, "delete", sandbox_id, "--blocking"])
        if result.returncode != 0:
            eprint(f"WARNING: cleanup failed for {sandbox_id}: {sanitize_error(result.stdout)}")


def timed_command(
    cmd: Sequence[str],
    *,
    operation: str,
    background: str | int = "-",
    burst: str | int = "-",
    repeat: str | int = "-",
    index: int = 1,
    log_file: Path,
    expected_boot_mode: str = "",
) -> CommandResult:
    started = now_ms()
    result = run_capture(cmd)
    elapsed = now_ms() - started
    log_file.parent.mkdir(parents=True, exist_ok=True)
    log_file.write_text(result.stdout)
    sandbox_id = extract_sandbox_id(result.stdout)
    rc = result.returncode
    error = "-" if rc == 0 else sanitize_error(result.stdout)
    if rc == 0 and operation.endswith("start") and not sandbox_id:
        rc = 97
        error = "command succeeded but did not print a sandbox ID"
    effective_boot_mode = extract_boot_mode(result.stdout)
    if rc == 0 and expected_boot_mode and effective_boot_mode and effective_boot_mode != expected_boot_mode:
        rc = 98
        error = f"effective boot mode {effective_boot_mode}, expected {expected_boot_mode}"
    if not sandbox_id:
        sandbox_id = "-"
    return CommandResult(
        operation=operation,
        background=background,
        burst=burst,
        repeat=repeat,
        index=index,
        ms=elapsed,
        rc=rc,
        sandbox_id=sandbox_id,
        error=error,
        log_file=str(log_file),
    )


def run_command_batch(
    commands: Sequence[tuple[list[str], dict[str, object]]],
    *,
    run_dir: Path,
    max_workers: int | None = None,
) -> tuple[int, list[CommandResult]]:
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "results").mkdir(exist_ok=True)
    (run_dir / "logs").mkdir(exist_ok=True)
    start_event = threading.Event()

    def one(item: tuple[list[str], dict[str, object]]) -> CommandResult:
        cmd, meta = item
        start_event.wait()
        return timed_command(cmd, **meta)  # type: ignore[arg-type]

    workers = max_workers or max(1, len(commands))
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        futures = [pool.submit(one, item) for item in commands]
        started = now_ms()
        start_event.set()
        results = [future.result() for future in futures]
        batch_ms = now_ms() - started

    for result in results:
        result_path = run_dir / "results" / f"{result.index}.tsv"
        result_path.write_text(command_result_tsv(result, include_error=True) + "\n")
    (run_dir / "batch_ms").write_text(f"{batch_ms}\n")
    return batch_ms, results


def command_result_tsv(result: CommandResult, *, include_error: bool) -> str:
    fields: list[object] = [
        result.operation,
        result.background,
        result.burst,
        result.repeat,
        result.index,
        result.ms,
        result.rc,
        result.sandbox_id,
    ]
    if include_error:
        fields.append(result.error)
    fields.append(result.log_file)
    return "\t".join(str(field) for field in fields)


def write_lines(path: Path, lines: Iterable[str]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as file:
        for line in lines:
            file.write(line)
            file.write("\n")


def valid_ids(results: Iterable[CommandResult]) -> list[str]:
    ids = []
    for result in results:
        if result.rc == 0 and UUID_RE.fullmatch(result.sandbox_id):
            ids.append(result.sandbox_id)
    return sorted(ids)


def cleanup_ids(cli: SandboxCli, ids: Iterable[str], *, enabled: bool = True) -> None:
    if not enabled:
        return
    for sandbox_id in sorted(set(id_ for id_ in ids if UUID_RE.fullmatch(id_))):
        cli.delete(sandbox_id)


def read_pool_counts(cli: SandboxCli) -> dict[str, str]:
    result = run_capture([*cli.prefix, cli.nodemgr, "status"])
    keys = ["status", "ready", "leased", "dirty", "resetting", "creating", "quarantined", "degraded"]
    values = {key: "-" for key in keys}
    if result.returncode != 0:
        return values
    for line in result.stdout.splitlines():
        if not line.startswith("Pool:"):
            continue
        for part in line.split():
            if "=" not in part:
                continue
            key, value = part.split("=", 1)
            if key in values:
                values[key] = value
        break
    return values


def read_ready_slots(cli: SandboxCli) -> int | None:
    ready = read_pool_counts(cli).get("ready", "-")
    return int(ready) if ready.isdigit() else None


def wait_for_ready_slots(cli: SandboxCli, required: int, timeout_secs: int, label: str) -> None:
    if required <= 0:
        return
    started = time.monotonic()
    while True:
        ready = read_ready_slots(cli)
        if ready is not None and ready >= required:
            print(f"  ready slots {ready}/{required}; {label}", flush=True)
            return
        if time.monotonic() - started >= timeout_secs:
            raise SystemExit(
                f"ERROR: timed out waiting for {required} ready network slots for {label}; "
                f"last ready={ready if ready is not None else 'unknown'}"
            )
        time.sleep(1)


def snapshot_resources(path: Path, cli: SandboxCli, phase: str, background: object, burst: object = "-", repeat: object = "-") -> None:
    load1 = "-"
    mem_total_kb = mem_avail_kb = None
    try:
        load1 = Path("/proc/loadavg").read_text().split()[0]
        for line in Path("/proc/meminfo").read_text().splitlines():
            if line.startswith("MemTotal:"):
                mem_total_kb = int(line.split()[1])
            elif line.startswith("MemAvailable:"):
                mem_avail_kb = int(line.split()[1])
    except (OSError, ValueError, IndexError):
        pass

    mem_used_gb = mem_total_gb = mem_pct = "-"
    if mem_total_kb and mem_avail_kb is not None:
        used = mem_total_kb - mem_avail_kb
        mem_used_gb = f"{used / 1048576:.2f}"
        mem_total_gb = f"{mem_total_kb / 1048576:.2f}"
        mem_pct = f"{(used * 100.0) / mem_total_kb:.2f}"

    fc_count = "0"
    pgrep = shutil.which("pgrep")
    if pgrep:
        result = run_capture([pgrep, "-c", "firecracker"])
        fc_count = result.stdout.strip() or "0"

    pool = read_pool_counts(cli)
    row = [
        time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        phase,
        background,
        burst,
        repeat,
        load1,
        mem_used_gb,
        mem_total_gb,
        mem_pct,
        fc_count,
        pool.get("status", "-"),
        pool.get("ready", "-"),
        pool.get("leased", "-"),
        pool.get("dirty", "-"),
        pool.get("resetting", "-"),
        pool.get("creating", "-"),
        pool.get("quarantined", "-"),
        pool.get("degraded", "-"),
    ]
    write_lines(path, ["\t".join(str(item) for item in row)])


def ssh_option_args(options: Sequence[str]) -> list[str]:
    args: list[str] = []
    for opt in options:
        args += ["-o", opt]
    return args


def normalize_target(target: str, ssh_user: str) -> str:
    return target if "@" in target else f"{ssh_user}@{target}"


def stage_and_run_remote_python(
    *,
    script_path: Path,
    target: str,
    ssh_options: Sequence[str],
    remote_args: Sequence[str],
) -> int:
    common_path = script_path.with_name("sbx_bench_common.py")
    suffix = f"{Path(script_path).stem}-{os.getpid()}"
    remote_dir = f"/tmp/space-bench-{suffix}"
    ssh_args = ssh_option_args(ssh_options)

    subprocess.run(["ssh", *ssh_args, target, "mkdir", "-p", remote_dir], check=True)
    subprocess.run(["scp", *ssh_args, str(script_path), str(common_path), f"{target}:{remote_dir}/"], check=True)
    remote_script = f"{remote_dir}/{script_path.name}"
    cmd = ["ssh", *ssh_args, target, "python3", remote_script, "--remote-run", *remote_args]
    return subprocess.run(cmd).returncode


def pull_results(
    *,
    target: str,
    ssh_options: Sequence[str],
    remote_output_dir: str,
    local_parent: str,
    files: Sequence[str],
    pull_raw: bool,
) -> None:
    local_dir = Path(local_parent) / Path(remote_output_dir).name
    local_dir.mkdir(parents=True, exist_ok=True)
    ssh_args = ssh_option_args(ssh_options)
    for file_name in files:
        subprocess.run(
            ["scp", "-q", *ssh_args, f"{target}:{remote_output_dir}/{file_name}", str(local_dir)],
            check=False,
        )
    if pull_raw:
        subprocess.run(
            ["scp", "-q", "-r", *ssh_args, f"{target}:{remote_output_dir}/runs", str(local_dir)],
            check=False,
        )
    print(f"Pulled compact results to: {local_dir}")


def add_common_ssh_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--ssh-user", default=os.environ.get("SSH_USER", "deploy"))
    parser.add_argument(
        "--ssh-option",
        action="append",
        default=[],
        help="Extra ssh option without -o. Repeatable.",
    )
    parser.add_argument("--pull-results", default="")
    parser.add_argument("--pull-raw", action="store_true")


def effective_ssh_options(extra: Sequence[str]) -> list[str]:
    opts = list(DEFAULT_SSH_OPTIONS)
    opts.extend(extra)
    return opts


def require_linux_remote_mode() -> None:
    if platform.system() != "Linux":
        raise SystemExit("ERROR: --remote-run benchmark mode must run on the worker node")
