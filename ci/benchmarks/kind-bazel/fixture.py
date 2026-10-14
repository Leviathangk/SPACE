#!/usr/bin/env python3
"""Bind a source fixture to its freshly built Kind artifact image."""

import argparse
import hashlib
import json
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--bazel", default="bazel")
    args = parser.parse_args()
    source = args.source.resolve()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    target = "//ci/benchmarks/kind-bazel/artifacts:image_tarball"
    subprocess.run([args.bazel, "build", target], cwd=source, check=True)
    files = subprocess.check_output(
        [args.bazel, "cquery", target, "--output=files"], cwd=source, text=True
    ).splitlines()
    if len(files) != 1:
        raise SystemExit("Expected exactly one complete Docker image archive")
    image = source / files[0]
    archive = output / "source.tar"
    # A fixture commit binds the exact worktree, including uncommitted Rust
    # changes. It is not presented as the upstream repository's commit.
    with tempfile.TemporaryDirectory(prefix="space-source-fixture-") as temporary:
        tree = Path(temporary)
        shutil.copytree(
            source,
            tree,
            dirs_exist_ok=True,
            ignore=shutil.ignore_patterns(
                ".git",
                ".cache",
                "bazel-*",
                "target",
                ".venv",
                "__pycache__",
                "*.pyc",
            ),
        )
        for argv in [
            ["git", "init", "--quiet"],
            ["git", "add", "."],
            [
                "git",
                "-c",
                "user.name=SPACE Benchmark",
                "-c",
                "user.email=benchmark@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "Source fixture",
            ],
        ]:
            subprocess.run(argv, cwd=tree, check=True)
        revision = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=tree, text=True
        ).strip()
        with tarfile.open(archive, "w") as bundle:
            for path in sorted(tree.iterdir()):
                bundle.add(path, arcname=path.name)
    manifest = {
        "source_sha256": sha256(archive),
        "artifact_sha256": sha256(image),
        "fixture_commit": revision,
        "artifact_archive": str(image),
    }
    archive.with_suffix(".json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({"source_archive": str(archive), **manifest}, indent=2))


if __name__ == "__main__":
    main()
