#!/usr/bin/env python3
"""Capture the complete Kind Cargo build and prepare its Bazel projection."""

import argparse
import importlib.util
import json
import re
import subprocess
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--builder", default="space-controller-benchmark")
    args = parser.parse_args()
    source = args.source.resolve()
    destination = args.destination.resolve()
    spec = importlib.util.spec_from_file_location(
        "controller_benchmark", source / "ci/benchmarks/controller-bazel/benchmark.py"
    )
    module = importlib.util.module_from_spec(spec)
    import sys

    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    module.CACHE.mkdir(parents=True, exist_ok=True)
    module.snapshot(destination, worktree=True, runtime_inputs=False)
    cache = destination / ".cache/controller-benchmark"
    (destination / ".dockerignore").write_text(
        (destination / ".dockerignore").read_text() + "\n.cache\nbazel-*\n"
    )
    common = (destination / "docker/Dockerfile.rust-build-common").read_text()
    chef = (
        common[: common.index("FROM chef-base AS planner")]
        + "ENV CARGO_BUILD_JOBS=16\n\n"
    )
    probe = common[
        common.index(
            "FROM public.ecr.aws/docker/library/debian:bookworm-slim AS binary-downloader-base"
        ) : common.index("FROM scratch AS build-artifacts-worker")
    ]
    kind = (destination / "docker/Dockerfile.rust-build-kind").read_text()
    kind = kind.replace("FROM ${SPACE_RUST_CHEF_IMAGE}", "FROM chef-base")
    kind = kind.replace(
        "FROM ${SPACE_GRPC_HEALTH_PROBE_IMAGE}", "FROM grpc-health-probe-downloader"
    )
    # Only instrumentation changes: preserve the exact feature/bin lists and
    # the separate supervisor invocation of the production Kind producer.
    kind = kind.replace(
        "RUN cargo build --locked --release",
        "RUN cargo build --locked --release --message-format=json",
    )
    kind = re.sub(
        r"\n\s*\nRUN bash scripts/build-runtime-supervisor.sh",
        " > /benchmark-main-profile.jsonl\n\nRUN bash scripts/build-runtime-supervisor.sh",
        kind,
    )
    kind = kind.replace(
        "cargo build --locked --release \\",
        "cargo build --locked --release --message-format=json \\",
    )
    kind = re.sub(
        r"\n\s*\nFROM grpc-health-probe-downloader AS grpc-health-probe",
        " > /benchmark-supervisor-profile.jsonl\n\nRUN cargo metadata --offline --locked --no-deps --format-version=1 > /benchmark-metadata.json && cat /benchmark-main-profile.jsonl /benchmark-supervisor-profile.jsonl > /benchmark-profile.jsonl\n\nFROM grpc-health-probe-downloader AS grpc-health-probe",
        kind,
    )
    text = (
        chef
        + probe
        + kind
        + """
FROM scratch AS benchmark-profile
COPY --from=build /benchmark-profile.jsonl /benchmark-metadata.json /
COPY --from=build-artifacts-kind /artifacts /artifacts
"""
    )
    dockerfile = destination / "docker/Dockerfile.kind-benchmark"
    dockerfile.write_text(text)
    profile = cache / "cargo-profile"
    log = cache / "kind-cargo-profile.log"
    started = time.monotonic()
    with log.open("w") as stream:
        result = subprocess.run(
            [
                "docker",
                "buildx",
                "build",
                "--builder",
                args.builder,
                "--platform",
                "linux/amd64",
                "--progress=plain",
                "--provenance=false",
                "--build-arg",
                "SPACE_SCCACHE_ENABLED=false",
                "--file",
                str(dockerfile),
                "--target",
                "benchmark-profile",
                "--output",
                f"type=local,dest={profile}",
                str(destination),
            ],
            stdout=stream,
            stderr=subprocess.STDOUT,
            check=False,
        )
    (cache / "kind-cargo-preparation.json").write_text(
        json.dumps(
            {
                "seconds": time.monotonic() - started,
                "returncode": result.returncode,
                "boundary": "profile preparation, includes builder bootstrap; not a paired benchmark",
            },
            indent=2,
        )
        + "\n"
    )
    if result.returncode:
        raise SystemExit(f"Cargo preparation failed; see {log}")
    for command in [
        ["python3", "ci/benchmarks/controller-bazel/profile.py"],
        [
            "uv",
            "--no-config",
            "run",
            "--frozen",
            "ci/benchmarks/controller-bazel/resolver.py",
        ],
        ["python3", "ci/benchmarks/controller-bazel/generate.py"],
    ]:
        subprocess.run(command, cwd=destination, check=True)
    subprocess.run(
        [
            "python3",
            "ci/benchmarks/kind-bazel/artifacts.py",
            "--source",
            str(destination),
        ],
        cwd=destination,
        check=True,
    )
    from bootstrap import fingerprint

    metadata = json.loads((profile / "benchmark-metadata.json").read_text())
    records = []
    selected = set()
    for line in (profile / "benchmark-profile.jsonl").read_text().splitlines():
        artifact = json.loads(line)
        if artifact["reason"] == "build-finished":
            records.append({"reason": "build-finished"})
        elif artifact["reason"] == "compiler-artifact" and artifact["target"][
            "kind"
        ] != ["custom-build"]:
            selected.add(artifact["package_id"])
            records.append(
                {
                    "reason": "compiler-artifact",
                    "package_id": artifact["package_id"],
                    "target": {
                        key: artifact["target"][key] for key in ("kind", "name")
                    },
                    "features": artifact["features"],
                    "profile": {"opt_level": artifact["profile"]["opt_level"]},
                }
            )
    captured = {
        "schema": 1,
        "manifest_fingerprint": fingerprint(destination),
        "metadata": {
            "packages": [
                {
                    key: package[key]
                    for key in ("id", "name", "version", "manifest_path")
                }
                for package in metadata["packages"]
                if package["id"] in selected
            ]
        },
        "artifacts": records,
    }
    (destination / "ci/benchmarks/kind-bazel/cargo-profile.json").write_text(
        json.dumps(captured, separators=(",", ":")) + "\n"
    )
    print(destination)


if __name__ == "__main__":
    main()
