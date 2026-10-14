#!/usr/bin/env python3
"""Measure equivalent complete Kind images, serially, with warmed local caches."""

import argparse
import json
import subprocess
import time
from pathlib import Path

TARGET = "//ci/benchmarks/kind-bazel/artifacts:image_tarball"


def measure(command, source, output, name):
    log = output / (name + ".log")
    started = time.monotonic()
    with log.open("w") as stream:
        result = subprocess.run(
            command, cwd=source, stdout=stream, stderr=subprocess.STDOUT, check=False
        )
    value = {
        "name": name,
        "seconds": time.monotonic() - started,
        "returncode": result.returncode,
        "log": str(log),
    }
    print(json.dumps(value), flush=True)
    if result.returncode:
        raise SystemExit(f"Build failed: {log}")
    return value


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--bazel", default="bazel")
    parser.add_argument("--builder", default="space-controller-benchmark")
    parser.add_argument(
        "--marker", required=True, help="A fresh synthetic edit identifier"
    )
    parser.add_argument(
        "--cases",
        nargs="+",
        choices=["unchanged", "test-only", "controller", "controller-cli"],
        default=["unchanged", "test-only", "controller"],
    )
    args = parser.parse_args()
    if not args.marker.replace("-", "").replace("_", "").isalnum():
        parser.error(
            "marker must contain only letters, digits, hyphens and underscores"
        )
    source, output = args.source.resolve(), args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    cargo = [
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
        "docker/Dockerfile.kind-benchmark",
        "--target",
        "build-artifacts-kind",
        "--tag",
        "space-rust-build-kind:local",
        "--output",
        f"type=docker,dest={output / 'cargo.tar'}",
        ".",
    ]
    bazel = [args.bazel, "build", TARGET]
    measurements = []
    for backend, command in [("cargo", cargo), ("bazel", bazel)]:
        measurements.append(measure(command, source, output, backend + "-warmup"))
    for case in args.cases:
        relative = (
            None
            if case == "unchanged"
            else "crates/apps/cluster/controller/svc/src/main.rs"
        )
        # Use an existing cfg(test) module, not a new file that neither build
        # would ever have considered an input.
        if case == "test-only":
            relative = next(
                path.relative_to(source).as_posix()
                for path in sorted(
                    (source / "crates/libs/cluster/controller/server/src").rglob(
                        "tests.rs"
                    )
                )
            )
        path = source / relative if relative is not None else None
        original = path.read_bytes() if path is not None else None
        try:
            if path is not None:
                path.write_bytes(
                    original + f"\n// Build benchmark {args.marker}: {case}.\n".encode()
                )
            if case == "controller-cli":
                marker = b"SPACE cluster controller: leader-elected reconciler set"
                if original.count(marker) != 1:
                    raise SystemExit(
                        "Controller CLI description changed; update the benchmark edit"
                    )
                path.write_bytes(
                    original.replace(
                        marker, marker + b" [" + args.marker.encode() + b"]"
                    )
                )
            for backend, command in [("cargo", cargo), ("bazel", bazel)]:
                measured = list(command)
                if backend == "bazel":
                    measured += [
                        f"--execution_log_json_file={output / (case + '-actions.json')}"
                    ]
                measurements.append(
                    measure(measured, source, output, backend + "-" + case)
                )
        finally:
            if path is not None:
                path.write_bytes(original)
    (output / "results.json").write_text(
        json.dumps(
            {
                "boundary": "complete Docker artifact archive; local warmed caches; 16 logical jobs; serial trials",
                "cargo_compiler_cache": "BuildKit layers; sccache disabled as in the public Kind Dockerfile",
                "measurements": measurements,
            },
            indent=2,
        )
        + "\n"
    )


if __name__ == "__main__":
    main()
