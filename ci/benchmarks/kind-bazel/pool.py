#!/usr/bin/env python3
"""Lease invocation-owned Kind workers to independently scheduled Bazel tests."""

import argparse
import concurrent.futures
import fcntl
import hashlib
import json
import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from pathlib import Path


def command(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def stop_worker(name, owner):
    result = subprocess.run(
        [
            "docker",
            "inspect",
            "-f",
            '{{index .Config.Labels "space.bazel.pool"}}',
            name,
        ],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode == 0 and result.stdout.strip() == owner:
        command("docker", "rm", "--force", "--volumes", name, stdout=subprocess.DEVNULL)


def bootstrap(config, name, log):
    cgroup = (
        [
            "--cgroup-parent",
            config["cgroup_parent"],
            "--cgroupns=private",
            "--env",
            "SPACE_KIND_BAZEL_DELEGATE_CGROUP=1",
        ]
        if config.get("cgroup_parent")
        else ["--cgroupns=host"]
    )
    command(
        "docker",
        "run",
        "--detach",
        "--privileged",
        *cgroup,
        "--device",
        "/dev/kvm",
        "--name",
        name,
        "--label",
        f"space.bazel.pool={config['owner']}",
        "--volume",
        "/var/lib/docker",
        "--volume",
        "/var/lib/space-kind-data",
        "--volume",
        "/workspace",
        config["runner_image"],
        stdout=subprocess.DEVNULL,
    )
    command("docker", "cp", config["source_archive"], name + ":/run/space-source.tar")
    command(
        "docker", "cp", config["artifact_archive"], name + ":/run/space-artifacts.tar"
    )
    command(
        "docker",
        "exec",
        name,
        "tar",
        "--no-same-owner",
        "-xf",
        "/run/space-source.tar",
        "-C",
        "/workspace",
    )
    with log.open("a") as stream:
        command(
            "docker",
            "exec",
            name,
            "bash",
            "ci/benchmarks/kind-bazel/worker.sh",
            "bootstrap",
            stdout=stream,
            stderr=subprocess.STDOUT,
            timeout=2400,
        )


def scenario(surface, scenario_id):
    if "SPACE_KIND_BAZEL_POOL" not in os.environ:
        raise SystemExit("Use pool.py run to create an invocation-owned Kind pool")
    pool = Path(os.environ["SPACE_KIND_BAZEL_POOL"])
    config = json.loads((pool / "config.json").read_text())
    while True:
        if (pool / "bootstrap.failed").exists():
            raise SystemExit(
                "Kind fixture bootstrap failed; inspect the worker bootstrap log"
            )
        assignments = config.get("assignments")
        slots = (
            [assignments[surface + ":" + scenario_id]]
            if assignments
            else range(config["workers"])
        )
        for slot in slots:
            with (pool / f"worker-{slot}.lock").open("a") as lock:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    continue
                name = f"space-bazel-{config['owner']}-{slot}"
                ready = pool / f"worker-{slot}.ready"
                log = pool / f"worker-{slot}.bootstrap.log"
                active = pool / f"worker-{slot}.active"
                # A timed-out test can die before its docker-exec child. A
                # surviving lease marker forces a reset before the next case.
                if active.exists():
                    ready.unlink(missing_ok=True)
                    stop_worker(name, config["owner"])
                    active.unlink()
                active.write_text(surface + ":" + scenario_id)
                try:
                    if not ready.exists():
                        stop_worker(name, config["owner"])
                        try:
                            bootstrap(config, name, log)
                        except BaseException:
                            (pool / "bootstrap.failed").write_text(str(slot))
                            raise
                        ready.touch()
                    started = time.monotonic()
                    result = subprocess.run(
                        [
                            "docker",
                            "exec",
                            name,
                            "bash",
                            "ci/benchmarks/kind-bazel/worker.sh",
                            "scenario",
                            surface,
                            scenario_id,
                        ],
                        check=False,
                    )
                    print(
                        json.dumps(
                            {
                                "surface": surface,
                                "scenario": scenario_id,
                                "worker": slot,
                                "seconds": time.monotonic() - started,
                                "returncode": result.returncode,
                            }
                        ),
                        flush=True,
                    )
                    # A failed scenario may leave shared controller settings
                    # behind. Its next lease starts from a fresh baseline.
                    if result.returncode:
                        ready.unlink(missing_ok=True)
                        stop_worker(name, config["owner"])
                    return result.returncode
                except BaseException:
                    ready.unlink(missing_ok=True)
                    stop_worker(name, config["owner"])
                    if log.exists():
                        print(log.read_text()[-12000:], flush=True)
                    raise
                finally:
                    active.unlink(missing_ok=True)
        time.sleep(0.1)


def target_pair(target):
    prefix = "//ci/benchmarks/kind-bazel:"
    if not target.startswith(prefix):
        raise ValueError("Fixed plans require explicit Kind scenario labels")
    surface, scenario_id = target.removeprefix(prefix).split("__", 1)
    if surface not in ("ecw", "native") or not scenario_id.replace("_", "").isalnum():
        raise ValueError("Invalid scenario target")
    return surface, scenario_id.replace("__", ".")


def fixed_tests(args, environment, partitions):
    failed = threading.Event()

    def partition(slot, targets):
        for target in targets:
            if failed.is_set():
                return 1
            surface, scenario_id = target_pair(target)
            log = args.output / "targets" / (target.split(":")[-1] + ".log")
            with log.open("w") as stream:
                try:
                    result = subprocess.run(
                        [
                            sys.executable,
                            str(Path(__file__).resolve()),
                            "scenario",
                            surface,
                            scenario_id,
                        ],
                        env=environment,
                        stdout=stream,
                        stderr=subprocess.STDOUT,
                        check=False,
                        timeout=3600,
                    )
                    code = result.returncode
                except subprocess.TimeoutExpired:
                    code = 124
            print(
                json.dumps({"target": target, "worker": slot, "returncode": code}),
                flush=True,
            )
            if code:
                failed.set()
                return 1
        return 0

    (args.output / "targets").mkdir()
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as executor:
        return max(executor.map(lambda item: partition(*item), enumerate(partitions)))


def run(args):
    if not Path("/dev/kvm").exists():
        raise SystemExit("This suite needs a Linux KVM worker; /dev/kvm is unavailable")
    if args.workers < 1:
        raise SystemExit("--workers must be positive")
    command("docker", "info", stdout=subprocess.DEVNULL)
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="space-bazel-pool-") as temporary:
        pool = Path(temporary)
        owner = uuid.uuid4().hex[:16]
        config = {
            "owner": owner,
            "workers": args.workers,
            "runner_image": args.runner_image,
            "cgroup_parent": args.cgroup_parent,
            "source_archive": str(args.source_archive.resolve()),
            "artifact_archive": str(args.artifact_archive.resolve()),
            "source_sha256": digest(args.source_archive),
            "artifact_sha256": digest(args.artifact_archive),
        }
        manifest = json.loads(args.source_archive.with_suffix(".json").read_text())
        if any(
            config[key] != manifest[key] for key in ("source_sha256", "artifact_sha256")
        ):
            raise SystemExit(
                "Source/image fixture digest mismatch; regenerate with fixture.py"
            )
        partitions = None
        if args.fixed_plan:
            partitions = json.loads(args.fixed_plan.read_text())
            flattened = [target for targets in partitions for target in targets]
            if (
                len(partitions) != args.workers
                or len(flattened) != len(set(flattened))
                or set(flattened) != set(args.targets)
            ):
                raise SystemExit(
                    "Fixed plan must cover each selected target exactly once"
                )
            config["assignments"] = {
                ":".join(target_pair(target)): slot
                for slot, targets in enumerate(partitions)
                for target in targets
            }
        (pool / "config.json").write_text(json.dumps(config))
        environment = os.environ | {"SPACE_KIND_BAZEL_POOL": str(pool)}
        # Every selected scenario must execute, even when a previous invocation
        # reported green. The live kernel and cluster aren't cacheable inputs.
        argv = [
            args.bazel,
            "test",
            *args.targets,
            f"--local_test_jobs={args.workers}",
            "--test_strategy=standalone",
            "--nocache_test_results",
            "--keep_going",
            "--test_output=errors",
            "--test_env=SPACE_KIND_BAZEL_POOL",
            "--test_env=PATH",
            "--test_env=DOCKER_HOST",
            "--test_env=DOCKER_CONTEXT",
        ]
        if args.stop_on_failure:
            argv.append("--notest_keep_going")
        process = None
        returncode = None
        setup_finished = None
        tests_finished = None

        def interrupted(signum, frame):
            if process is not None:
                process.send_signal(signum)
            raise KeyboardInterrupt

        old_handlers = {
            s: signal.signal(s, interrupted) for s in (signal.SIGINT, signal.SIGTERM)
        }
        started = time.monotonic()
        try:
            if args.prewarm:

                def prepare(slot):
                    bootstrap(
                        config,
                        f"space-bazel-{owner}-{slot}",
                        pool / f"worker-{slot}.bootstrap.log",
                    )
                    (pool / f"worker-{slot}.ready").touch()

                with concurrent.futures.ThreadPoolExecutor(
                    max_workers=args.workers
                ) as executor:
                    list(executor.map(prepare, range(args.workers)))
            setup_finished = time.monotonic()
            if partitions is not None:
                returncode = fixed_tests(args, environment, partitions)
            else:
                process = subprocess.Popen(argv, env=environment)
                returncode = process.wait()
            tests_finished = time.monotonic()
            return returncode
        finally:
            if process is not None and process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=30)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            for s, handler in old_handlers.items():
                signal.signal(s, handler)
            for slot in range(args.workers):
                name = f"space-bazel-{owner}-{slot}"
                with (args.output / f"worker-{slot}.pods.txt").open("w") as stream:
                    try:
                        subprocess.run(
                            [
                                "docker",
                                "exec",
                                name,
                                "kubectl",
                                "--context",
                                "kind-space-local",
                                "-n",
                                "space",
                                "get",
                                "pods",
                                "-o",
                                "wide",
                            ],
                            stdout=stream,
                            stderr=subprocess.STDOUT,
                            timeout=15,
                            check=False,
                        )
                    except subprocess.TimeoutExpired:
                        stream.write("Pod diagnostics timed out\n")
                stop_worker(name, owner)
                log = pool / f"worker-{slot}.bootstrap.log"
                if log.exists():
                    (args.output / log.name).write_text(log.read_text())
            (args.output / "run.json").write_text(
                json.dumps(
                    {
                        "seconds": time.monotonic() - started,
                        "returncode": returncode,
                        "schedule": "fixed" if partitions is not None else "dynamic",
                        "prewarmed": args.prewarm,
                        "bootstrap_seconds": setup_finished - started
                        if args.prewarm and setup_finished
                        else None,
                        "execution_seconds": tests_finished - setup_finished
                        if tests_finished
                        else None,
                        "cleanup_seconds": time.monotonic() - tests_finished
                        if tests_finished
                        else None,
                        "workers": args.workers,
                        "cgroup_parent": args.cgroup_parent,
                        "targets": args.targets,
                        "source_sha256": config["source_sha256"],
                        "artifact_sha256": config["artifact_sha256"],
                    },
                    indent=2,
                )
                + "\n"
            )


def main():
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    single = subparsers.add_parser("scenario")
    single.add_argument("surface", choices=["ecw", "native"])
    single.add_argument("scenario")
    suite = subparsers.add_parser("run")
    suite.add_argument("--bazel", default="bazel")
    suite.add_argument("--workers", type=int, default=1)
    suite.add_argument(
        "--prewarm",
        action="store_true",
        help="Prepare every stack before timing target execution",
    )
    suite.add_argument(
        "--fixed-plan",
        type=Path,
        help="JSON partitions for the fixed-assignment comparison adapter",
    )
    suite.add_argument("--stop-on-failure", action="store_true")
    suite.add_argument(
        "--cgroup-parent",
        help="Host cgroup containing every worker; use private cgroup namespaces to retain inherited limits",
    )
    suite.add_argument("--runner-image", default="space-bazel-kind-runner:local")
    suite.add_argument("--source-archive", type=Path, required=True)
    suite.add_argument("--artifact-archive", type=Path, required=True)
    suite.add_argument("--output", type=Path, required=True)
    suite.add_argument(
        "targets", nargs="+", help="Bazel scenario targets or :functional"
    )
    args = parser.parse_args()
    return (
        scenario(args.surface, args.scenario)
        if args.command == "scenario"
        else run(args)
    )


if __name__ == "__main__":
    raise SystemExit(main())
