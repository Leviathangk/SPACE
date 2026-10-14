#!/usr/bin/env python3
"""Generate the complete Kind artifact image from its captured Cargo inventory."""

import argparse
import json
import re
from pathlib import Path


def rule(kind, **attributes):
    return (
        kind
        + "(\n"
        + "".join(
            f"    {name} = {json.dumps(value)},\n" for name, value in attributes.items()
        )
        + ")\n\n"
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=Path, required=True)
    args = parser.parse_args()
    root = args.source.resolve()
    cache = root / ".cache/controller-benchmark"
    metadata = json.loads((cache / "cargo-profile/benchmark-metadata.json").read_text())
    packages = {package["id"]: package for package in metadata["packages"]}
    binaries = {}
    for line in (
        (cache / "cargo-profile/benchmark-profile.jsonl").read_text().splitlines()
    ):
        artifact = json.loads(line)
        if artifact.get("reason") != "compiler-artifact" or artifact["target"][
            "kind"
        ] != ["bin"]:
            continue
        package = packages[artifact["package_id"]]
        directory = (
            Path(package["manifest_path"]).parent.as_posix().removeprefix("/workspace/")
        )
        binaries[artifact["target"]["name"]] = (
            "//" + directory + ":" + Path(directory).name
        )
    expected = set(
        re.findall(
            r"/artifacts/bin/([^\s]+)",
            (root / "docker/Dockerfile.rust-build-kind").read_text(),
        )
    ) - {"grpc_health_probe"}
    if set(binaries) != expected:
        raise SystemExit(f"Binary inventory differs: {set(binaries) ^ expected}")
    package = root / "ci/benchmarks/kind-bazel/artifacts"
    package.mkdir(parents=True, exist_ok=True)
    common = (root / "docker/Dockerfile.rust-build-common").read_text()
    probe_version = re.search(r"ARG GRPC_HEALTH_PROBE_VERSION=(\S+)", common)[1]
    probe_sha = re.search(
        r"amd64\) asset=grpc_health_probe-linux-amd64; sha256=([0-9a-f]{64})", common
    )[1]
    (package.parent / "probe.MODULE.bazel").write_text(
        'http_file = use_repo_rule("@bazel_tools//tools/build_defs/repo:http.bzl", "http_file")\n\n'
        + 'http_file(\n    name = "benchmark_health_probe",\n'
        + '    downloaded_file_path = "grpc_health_probe",\n    executable = True,\n'
        + f'    sha256 = "{probe_sha}",\n'
        + f'    urls = ["https://github.com/grpc-ecosystem/grpc-health-probe/releases/download/{probe_version}/grpc_health_probe-linux-amd64"],\n)\n'
    )
    module = root / "MODULE.bazel"
    include = 'include("//ci/benchmarks/kind-bazel:probe.MODULE.bazel")\n'
    if include not in module.read_text():
        module.write_text(module.read_text() + "\n" + include)
    text = """load("@rules_oci//oci:defs.bzl", "oci_image", "oci_load")
load("@rules_pkg//pkg:tar.bzl", "pkg_tar")
load("//ci/benchmarks/controller-bazel:variants.bzl", "supervisor_profile")

"""
    text += rule(
        "supervisor_profile",
        name="supervisor",
        actual=binaries["space-runtime-supervisor-svc"],
    )
    binaries["space-runtime-supervisor-svc"] = ":supervisor"
    text += rule(
        "genrule",
        name="probe_binary",
        srcs=["@benchmark_health_probe//file"],
        outs=["grpc_health_probe"],
        cmd='cp "$(location @benchmark_health_probe//file)" "$@"',
    )
    layers = []
    for name, label in sorted(binaries.items()):
        text += rule(
            "genrule",
            name="copy_" + name,
            srcs=[label],
            outs=[name],
            cmd=f'cp "$(location {label})" "$@"',
        )
        layer = name + "_layer"
        layers.append(":" + layer)
        text += rule(
            "pkg_tar",
            name=layer,
            srcs=[":copy_" + name],
            package_dir="/artifacts/bin",
            mode="0755",
            extension="tar.gz",
            compression_level=6,
        )
    text += rule(
        "genrule",
        name="supervisor_compatibility",
        srcs=[":copy_space-runtime-supervisor-svc"],
        outs=["space-runtime-supervisor-compatibility"],
        cmd='sha256sum "$(location :copy_space-runtime-supervisor-svc)" | cut -d " " -f 1 > "$@"',
    )
    text += rule(
        "pkg_tar",
        name="probe_layer",
        srcs=[":probe_binary"],
        package_dir="/artifacts/bin",
        mode="0755",
        extension="tar.gz",
        compression_level=6,
    )
    text += rule(
        "pkg_tar",
        name="compatibility",
        srcs=[":supervisor_compatibility"],
        package_dir="/artifacts/etc",
        mode="0644",
        extension="tar.gz",
        compression_level=6,
    )
    text += rule(
        "oci_image",
        name="image",
        os="linux",
        architecture="amd64",
        workdir="/",
        env={"PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"},
        labels={
            "org.opencontainers.image.title": "space-rust-build-kind",
            "org.opencontainers.image.description": "SPACE Rust build artifacts for the local kind stack: the union of every production artifact set from one build.",
        },
        tars=layers + [":probe_layer", ":compatibility"],
    )
    text += rule(
        "oci_load",
        name="load",
        image=":image",
        repo_tags=["space-rust-build-kind:local"],
    )
    text += rule(
        "filegroup",
        name="image_tarball",
        srcs=[":load"],
        output_group="tarball",
        visibility=["//visibility:public"],
    )
    (package / "BUILD.bazel").write_text(text)
    print(
        f"Generated image with {len(binaries)} Rust binaries and the pinned gRPC probe"
    )


if __name__ == "__main__":
    main()
