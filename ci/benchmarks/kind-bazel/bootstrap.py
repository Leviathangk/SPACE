#!/usr/bin/env python3
"""Prepare the locked Bazel catalog without recompiling the Cargo reference."""

import argparse
import hashlib
import json
import subprocess
from pathlib import Path


def fingerprint(root):
    files = [
        root / name
        for name in (
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "docker/Dockerfile.rust-build-kind",
            "docker/Dockerfile.rust-build-common",
        )
    ] + sorted((root / "crates").rglob("Cargo.toml"))
    digest = hashlib.sha256()
    for path in files:
        digest.update(path.relative_to(root).as_posix().encode() + b"\0")
        digest.update(path.read_bytes() + b"\0")
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--native-jobs", type=int, default=16)
    args = parser.parse_args()
    root = args.source.resolve()
    if args.native_jobs < 1:
        parser.error("native-jobs must be positive")
    captured = json.loads(
        (root / "ci/benchmarks/kind-bazel/cargo-profile.json").read_text()
    )
    if fingerprint(root) != captured["manifest_fingerprint"]:
        raise SystemExit(
            "Cargo manifests, lockfile, toolchain or Kind build plan changed; refresh the independent profile with prepare.py"
        )
    cache = root / ".cache/controller-benchmark/cargo-profile"
    cache.mkdir(parents=True, exist_ok=True)
    (cache / "benchmark-metadata.json").write_text(json.dumps(captured["metadata"]))
    (cache / "benchmark-profile.jsonl").write_text(
        "\n".join(json.dumps(value) for value in captured["artifacts"]) + "\n"
    )
    for command in [
        ["python3", "ci/benchmarks/controller-bazel/profile.py"],
        [
            "uv",
            "--no-config",
            "run",
            "--frozen",
            "ci/benchmarks/controller-bazel/resolver.py",
            "--native-jobs",
            str(args.native_jobs),
        ],
        ["python3", "ci/benchmarks/controller-bazel/generate.py"],
        ["python3", "ci/benchmarks/kind-bazel/artifacts.py", "--source", str(root)],
    ]:
        subprocess.run(command, cwd=root, check=True)
    print("Locked full-Kind profile prepared; no Cargo compilation required")


if __name__ == "__main__":
    main()
