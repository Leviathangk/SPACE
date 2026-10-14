#!/usr/bin/env python3
"""Prepare the existing controller runtime for both image-build implementations."""

import argparse
import subprocess
from pathlib import Path

root = Path(__file__).resolve().parents[3]
parser = argparse.ArgumentParser()
parser.add_argument("--builder", required=True)
args = parser.parse_args()
cache = root / ".cache/controller-benchmark"
cache.mkdir(parents=True, exist_ok=True)
source = (root / "docker/Dockerfile.cluster-controller").read_text()
runtime = source[source.index("FROM public.ecr.aws/docker/library/debian:") :]
lines = runtime.splitlines()
runtime = (
    "\n".join(
        line
        for line in lines
        if not (
            line.startswith("COPY --from=rust-build ")
            or line.startswith("ARG SPACE_GIT_SHA=")
            or line.startswith("LABEL org.opencontainers.image.revision=")
            or line.startswith("RUN mkdir -p /etc/space && printf")
        )
    )
    + "\n"
)
(cache / "Dockerfile.runtime").write_text(runtime)
revision = subprocess.check_output(
    ["git", "rev-parse", "HEAD"], cwd=root, text=True
).strip()
(cache / "revision.txt").write_text(revision + "\n")
(cache / "labels.txt").write_text(
    "org.opencontainers.image.revision=" + revision + "\n"
)
subprocess.run(
    [
        "docker",
        "buildx",
        "build",
        "--builder",
        args.builder,
        "--platform",
        "linux/amd64",
        "--provenance=false",
        "--progress=plain",
        "--file",
        str(cache / "Dockerfile.runtime"),
        "--output",
        f"type=oci,dest={cache / 'runtime.oci.tar'}",
        str(root),
    ],
    check=True,
)
