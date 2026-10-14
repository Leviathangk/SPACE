#!/usr/bin/env python3
"""Lease invocation-owned Kind workers to independently scheduled Bazel tests."""

import argparse
import fcntl
import hashlib
import json
import os
import signal
import subprocess
import tempfile
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
    command(
        "docker",
        "run",
        "--detach",
        "--privileged",
        "--cgroupns=host",
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
        for slot in range(config["workers"]):
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
        process = None

        def interrupted(signum, frame):
            if process is not None:
                process.send_signal(signum)
            raise KeyboardInterrupt

        old_handlers = {
            s: signal.signal(s, interrupted) for s in (signal.SIGINT, signal.SIGTERM)
        }
        started = time.monotonic()
        try:
            process = subprocess.Popen(argv, env=environment)
            returncode = process.wait()
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
                        "returncode": process.returncode
                        if process is not None
                        else None,
                        "workers": args.workers,
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
