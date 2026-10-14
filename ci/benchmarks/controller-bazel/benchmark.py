#!/usr/bin/env python3
"""Compare the real controller image builds in disposable source snapshots."""

import argparse
import hashlib
import json
import shutil
import subprocess
import tarfile
import time
from dataclasses import asdict, dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
PACKAGE = "ci/benchmarks/controller-bazel"
CACHE = ROOT / ".cache/controller-benchmark"


@dataclass
class Measurement:
    backend: str
    case: str
    seconds: float
    returncode: int
    log: str
    source_revision: str
    lto: str
    jobs: int
    output: str
    cache_mode: str
    source_mode: str = "archive"


def run(command, *, cwd=ROOT, log=None):
    if log is None:
        return subprocess.run(command, cwd=cwd, check=True)
    with log.open("w") as stream:
        return subprocess.run(
            command, cwd=cwd, stdout=stream, stderr=subprocess.STDOUT, check=False
        )


def snapshot(destination, worktree=False, runtime_inputs=True):
    destination.mkdir(parents=True, exist_ok=False)
    archive = CACHE / "source.tar"
    with archive.open("wb") as stream:
        subprocess.run(["git", "archive", "HEAD"], cwd=ROOT, stdout=stream, check=True)
    with tarfile.open(archive) as source:
        source.extractall(destination, filter="data")
    if worktree:
        tracked = subprocess.check_output(
            ["git", "diff", "HEAD", "--name-only", "-z"], cwd=ROOT
        ).split(b"\0")
        untracked = subprocess.check_output(
            ["git", "ls-files", "--others", "--exclude-standard", "-z"], cwd=ROOT
        ).split(b"\0")
        for raw in tracked + untracked:
            if not raw:
                continue
            relative = Path(raw.decode())
            original = ROOT / relative
            target = destination / relative
            if original.is_file():
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(original, target)
            elif target.is_file():
                target.unlink()
    for filename in [
        "MODULE.bazel",
        "MODULE.bazel.lock",
        "BUILD.bazel",
        ".bazelrc",
        ".bazelversion",
        ".bazelrc.dx",
    ]:
        if (ROOT / filename).exists():
            shutil.copy2(ROOT / filename, destination / filename)
    shutil.copytree(
        ROOT / PACKAGE,
        destination / PACKAGE,
        dirs_exist_ok=True,
        ignore=shutil.ignore_patterns("__pycache__"),
    )
    cache = destination / ".cache/controller-benchmark"
    cache.mkdir(parents=True)
    if (CACHE / "resolver").exists():
        shutil.copytree(CACHE / "resolver", cache / "resolver")
    if (CACHE / "cargo-features.json").exists():
        shutil.copy2(CACHE / "cargo-features.json", cache / "cargo-features.json")
    if runtime_inputs:
        for filename in ["runtime.oci.tar", "revision.txt", "labels.txt"]:
            shutil.copy2(CACHE / filename, cache / filename)
    if (cache / "cargo-features.json").exists():
        run(["python3", str(destination / PACKAGE / "generate.py")], cwd=destination)
    return destination


def baseline_dockerfile(source, seed):
    text = (source / "docker/Dockerfile.rust-build-common").read_text()
    text = text.replace(
        "FROM chef-base AS fetch-deps\n",
        "FROM chef-base AS fetch-deps\nENV SPACE_BENCHMARK_LOCAL_CACHE=1 CARGO_BUILD_JOBS=16\n",
    )
    text = text.replace(
        "RUN --mount=type=secret,id=aws_web_identity_token \\\n",
        "RUN --mount=type=cache,id=controller-benchmark,target=/benchmark-sccache "
        "--mount=type=bind,from=compiler-cache,target=/benchmark-seed \\\n"
        "    cp -a /benchmark-seed/. /benchmark-sccache/ && \\\n",
    )
    text = text.replace(
        "cargo build --locked --release", "cargo build --locked --release --verbose"
    )
    text += """
FROM build-cluster-controller AS benchmark-cache-stage
RUN --mount=type=cache,id=controller-benchmark,target=/benchmark-sccache \\
    mkdir /benchmark-cache-export && cp -a /benchmark-sccache/. /benchmark-cache-export/
FROM scratch AS benchmark-compiler-cache
COPY --from=benchmark-cache-stage /benchmark-cache-export/ /
"""
    output = source / ".cache/controller-benchmark/Dockerfile.baseline"
    output.write_text(text)
    wrapper = source / "docker/run-cargo-with-sccache.sh"
    original = wrapper.read_text()
    local = """
if [ "${SPACE_BENCHMARK_LOCAL_CACHE:-}" = 1 ]; then
    export RUSTC_WRAPPER=sccache SCCACHE_DIR=/benchmark-sccache
    export SCCACHE_BASEDIRS=/workspace CARGO_INCREMENTAL=0
    sccache --zero-stats
    set +e
    "$@"
    result=$?
    sccache --show-stats
    exit "$result"
fi
"""
    if "SPACE_BENCHMARK_LOCAL_CACHE" not in original:
        wrapper.write_text(original.replace("set -eu\n", "set -eu\n" + local, 1))
    return output


def cargo(
    source,
    builder,
    seed,
    *,
    layer_cache=None,
    export_cache=None,
    cache_only=False,
    cold=False,
    cache_id="controller-benchmark",
):
    cache = source / ".cache/controller-benchmark"
    dockerfile = baseline_dockerfile(source, seed)
    dockerfile.write_text(
        dockerfile.read_text().replace(
            "id=controller-benchmark,", "id=" + cache_id + ","
        )
    )
    destination = cache / ("compiler-cache" if cache_only else "artifacts")
    command = [
        "docker",
        "buildx",
        "build",
        "--builder",
        builder,
        "--platform",
        "linux/amd64",
        "--progress=plain",
        "--provenance=false",
        "--file",
        str(dockerfile),
        "--target",
        "benchmark-compiler-cache"
        if cache_only
        else "build-artifacts-cluster-controller",
        "--build-arg",
        "SPACE_RUST_ARTIFACT_SET=cluster-controller",
        "--build-arg",
        "SPACE_CARGO_RELEASE_LTO=false",
        "--build-context",
        f"compiler-cache={seed}",
        "--output",
        f"type=local,dest={destination}",
    ]
    if cache_only:
        command += ["--no-cache-filter", "benchmark-cache-stage"]
    if cold:
        command += ["--no-cache-filter", "fetch-deps,build-cluster-controller"]
    if layer_cache:
        command += ["--cache-from", f"type=local,src={layer_cache}"]
    if export_cache:
        command += ["--cache-to", f"type=local,dest={export_cache},mode=max"]
    result = run(
        command + [str(source)],
        log=cache / ("cargo-cache-export.log" if cache_only else "cargo-build.log"),
    )
    if result.returncode or cache_only:
        return result
    revision = (cache / "revision.txt").read_text().strip()
    command = [
        "docker",
        "buildx",
        "build",
        "--builder",
        builder,
        "--platform",
        "linux/amd64",
        "--progress=plain",
        "--provenance=false",
        "--file",
        str(source / "docker/Dockerfile.cluster-controller"),
        "--build-context",
        f"controller-artifacts={destination}",
        "--build-arg",
        "SPACE_RUST_BUILD_IMAGE=controller-artifacts",
        "--build-arg",
        f"SPACE_GIT_SHA={revision}",
        "--tag",
        "space-controller-cargo:benchmark",
        "--output",
        f"type=docker,dest={cache / 'image.tar'}",
        str(source),
    ]
    if (CACHE / "buildkit-image-cache").exists():
        command += ["--cache-from", f"type=local,src={CACHE / 'buildkit-image-cache'}"]
    return run(command, log=cache / "cargo-image.log")


def export_image_cache(source, builder, destination):
    cache = source / ".cache/controller-benchmark"
    revision = (cache / "revision.txt").read_text().strip()
    result = run(
        [
            "docker",
            "buildx",
            "build",
            "--builder",
            builder,
            "--platform",
            "linux/amd64",
            "--progress=plain",
            "--provenance=false",
            "--file",
            str(source / "docker/Dockerfile.cluster-controller"),
            "--build-context",
            f"controller-artifacts={cache / 'artifacts'}",
            "--build-arg",
            "SPACE_RUST_BUILD_IMAGE=controller-artifacts",
            "--build-arg",
            f"SPACE_GIT_SHA={revision}",
            "--cache-to",
            f"type=local,dest={destination},mode=max",
            "--output",
            "type=cacheonly",
            str(source),
        ],
        log=cache / "cargo-image-cache-export.log",
    )
    if result.returncode:
        raise SystemExit(result.returncode)


def prime_compiler_cache(source, builder, seed, layer_cache, cache_id):
    dockerfile = baseline_dockerfile(source, seed)
    dockerfile.write_text(
        dockerfile.read_text()
        + f"""
FROM chef-base AS benchmark-prime-compiler-cache
RUN --mount=type=cache,id={cache_id},target=/benchmark-sccache --mount=type=bind,from=compiler-cache,target=/benchmark-seed cp -a /benchmark-seed/. /benchmark-sccache/
"""
    )
    command = [
        "docker",
        "buildx",
        "build",
        "--builder",
        builder,
        "--platform",
        "linux/amd64",
        "--progress=plain",
        "--file",
        str(dockerfile),
        "--target",
        "benchmark-prime-compiler-cache",
        "--build-context",
        f"compiler-cache={seed}",
        "--output",
        "type=cacheonly",
    ]
    if layer_cache.exists():
        command += ["--cache-from", f"type=local,src={layer_cache}"]
    result = run(
        command + [str(source)],
        log=source / ".cache/controller-benchmark/cargo-prime.log",
    )
    if result.returncode:
        raise SystemExit(result.returncode)


def bazel(source, executable, disk_cache):
    cache = source / ".cache/controller-benchmark"
    return run(
        [
            executable,
            "build",
            f"//{PACKAGE}:image_tarball",
            f"--disk_cache={disk_cache}",
            f"--profile={cache / 'bazel-profile.json'}",
            f"--execution_log_json_file={cache / 'bazel-actions.json'}",
        ],
        cwd=source,
        log=cache / "bazel-build.log",
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("backend", choices=["cargo", "bazel"])
    parser.add_argument(
        "--case",
        choices=["reference", "warm", "fresh", "unrelated", "controller", "provenance"],
        default="reference",
    )
    parser.add_argument("--builder", default="space-controller-benchmark")
    parser.add_argument("--bazel", default="bazel")
    parser.add_argument("--export-caches", action="store_true")
    parser.add_argument("--run-id", default="matched")
    parser.add_argument("--variant", default="")
    parser.add_argument("--input-run-id", default="")
    parser.add_argument("--prepare-only", action="store_true")
    parser.add_argument("--capture-profile", action="store_true")
    parser.add_argument(
        "--worktree",
        action="store_true",
        help="Include uncommitted source changes in disposable snapshots",
    )
    args = parser.parse_args()
    global CACHE
    if not args.run_id.replace("-", "").replace("_", "").isalnum():
        parser.error("run-id must be letters, digits, hyphens or underscores")
    if args.variant and not args.variant.replace("-", "").replace("_", "").isalnum():
        parser.error("variant must be letters, digits, hyphens or underscores")
    if (
        args.input_run_id
        and not args.input_run_id.replace("-", "").replace("_", "").isalnum()
    ):
        parser.error("input-run-id must be letters, digits, hyphens or underscores")
    CACHE = CACHE / args.run_id
    CACHE.mkdir(parents=True, exist_ok=True)
    for name in [
        "runtime.oci.tar",
        "revision.txt",
        "labels.txt",
        "cargo-features.json",
    ]:
        original = ROOT / ".cache/controller-benchmark" / name
        if original.exists():
            shutil.copy2(original, CACHE / name)
    if (ROOT / ".cache/controller-benchmark/resolver").exists():
        shutil.copytree(
            ROOT / ".cache/controller-benchmark/resolver",
            CACHE / "resolver",
            dirs_exist_ok=True,
        )
    if (
        args.case == "reference"
        and not args.prepare_only
        and not args.capture_profile
        and (CACHE / f"{args.backend}-reference.json").exists()
    ):
        parser.error("cold reference already exists; use a new run-id")
    case_source = "reference" if args.case == "warm" else args.case
    suffix = "-" + args.variant if args.variant else ""
    location = CACHE / f"{args.backend}-{case_source}{suffix}"
    source = (
        location if location.exists() else snapshot(location, worktree=args.worktree)
    )
    mutation = source / ".cache/controller-benchmark/case-mutated"
    if args.case == "unrelated" and not mutation.exists():
        with (source / "crates/apps/api/svc/src/main.rs").open("a") as stream:
            stream.write(
                f"\nconst _: () = {{ /* benchmark {args.input_run_id or args.run_id} {args.variant} */ }};\n"
            )
    elif args.case == "controller" and not mutation.exists():
        with (source / "crates/apps/cluster/controller/svc/src/main.rs").open(
            "a"
        ) as stream:
            stream.write(
                f"\nconst _: () = {{ /* benchmark {args.input_run_id or args.run_id} {args.variant} */ }};\n"
            )
    elif args.case == "provenance" and not mutation.exists():
        marker = hashlib.sha256(
            f"{args.input_run_id or args.run_id}/{args.variant}".encode()
        ).hexdigest()[:40]
        (source / ".cache/controller-benchmark/revision.txt").write_text(marker + "\n")
        (source / ".cache/controller-benchmark/labels.txt").write_text(
            "org.opencontainers.image.revision=" + marker + "\n"
        )
    if args.case in {"unrelated", "controller", "provenance"}:
        mutation.touch()
    empty_seed = CACHE / "empty-compiler-cache"
    empty_seed.mkdir(exist_ok=True)
    seed = empty_seed
    layer_cache = CACHE / "buildkit-cache"
    if args.capture_profile:
        if args.backend != "cargo":
            parser.error("capture-profile requires cargo")
        dockerfile = baseline_dockerfile(source, empty_seed)
        dockerfile.write_text(
            dockerfile.read_text()
            + """
FROM build-cluster-controller AS benchmark-profile-stage
RUN cargo build --offline --locked --release --message-format=json -F pplx-space-cluster-controller/otel,pplx-space-cluster-controller/datadog --bin space-cluster-controller > /benchmark-profile.jsonl && cargo metadata --offline --locked --no-deps --format-version=1 > /benchmark-metadata.json
FROM scratch AS benchmark-profile
COPY --from=benchmark-profile-stage /benchmark-profile.jsonl /benchmark-metadata.json /
"""
        )
        # The compiler-artifact JSON describes cached units too, so this need not rebuild.
        run(
            [
                "docker",
                "buildx",
                "build",
                "--builder",
                args.builder,
                "--platform",
                "linux/amd64",
                "--progress=plain",
                "--file",
                str(dockerfile),
                "--target",
                "benchmark-profile",
                "--build-arg",
                "SPACE_RUST_ARTIFACT_SET=cluster-controller",
                "--build-arg",
                "SPACE_CARGO_RELEASE_LTO=false",
                "--build-context",
                f"compiler-cache={seed}",
                "--output",
                f"type=local,dest={ROOT / '.cache/controller-benchmark/cargo-profile'}",
                str(source),
            ]
        )
        return
    if args.prepare_only:
        if args.backend == "bazel":
            run([args.bazel, "fetch", f"//{PACKAGE}:image_tarball"], cwd=source)
        print(source)
        return
    if (
        args.backend == "cargo"
        and args.case == "fresh"
        and (CACHE / "compiler-cache").exists()
    ):
        prime_compiler_cache(
            source,
            args.builder,
            CACHE / "compiler-cache",
            layer_cache,
            "controller-benchmark-" + args.run_id,
        )
    start = time.perf_counter()
    if args.backend == "bazel":
        setup = run(
            [args.bazel, "fetch", f"//{PACKAGE}:image_tarball"],
            cwd=source,
            log=source / ".cache/controller-benchmark/bazel-setup.log",
        )
        if setup.returncode:
            raise SystemExit(setup.returncode)
    if args.backend == "cargo":
        result = cargo(
            source,
            args.builder,
            seed,
            layer_cache=layer_cache
            if layer_cache.exists() and args.case != "reference"
            else None,
            export_cache=None,
            cold=args.case == "reference",
            cache_id="controller-benchmark-" + args.run_id,
        )
    else:
        result = bazel(source, args.bazel, CACHE / "bazel-disk-cache")
    seconds = time.perf_counter() - start
    saved_logs = source / ".cache/controller-benchmark" / ("logs-" + args.case)
    saved_logs.mkdir(exist_ok=True)
    for filename in [
        "cargo-build.log",
        "cargo-image.log",
        "bazel-build.log",
        "bazel-profile.json",
        "bazel-actions.json",
    ]:
        original = source / ".cache/controller-benchmark" / filename
        if original.exists():
            shutil.copy2(original, saved_logs / filename)
    measurement = Measurement(
        args.backend,
        args.case,
        seconds,
        result.returncode,
        str(saved_logs),
        (CACHE / "revision.txt").read_text().strip(),
        "false (local ThinLTO)",
        16,
        "Docker image archive; daemon load excluded",
        "empty compiler cache"
        if args.case == "reference"
        else "existing local compiler/layer/action cache",
        source_mode="worktree" if args.worktree else "archive",
    )
    (CACHE / f"{args.backend}-{args.case}.json").write_text(
        json.dumps(asdict(measurement), indent=2) + "\n"
    )
    print(json.dumps(asdict(measurement), indent=2))
    if result.returncode == 0 and args.backend == "cargo" and args.export_caches:
        cache_result = cargo(
            source,
            args.builder,
            empty_seed,
            layer_cache=layer_cache,
            cache_only=True,
            export_cache=layer_cache,
            cache_id="controller-benchmark-" + args.run_id,
        )
        if cache_result.returncode:
            raise SystemExit(cache_result.returncode)
        shutil.copytree(
            source / ".cache/controller-benchmark/compiler-cache",
            CACHE / "compiler-cache",
            dirs_exist_ok=True,
        )
        export_image_cache(source, args.builder, CACHE / "buildkit-image-cache")
    raise SystemExit(result.returncode)


if __name__ == "__main__":
    main()
