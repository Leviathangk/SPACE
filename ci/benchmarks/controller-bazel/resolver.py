#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = ["tomli-w==1.2.0"]
# ///
"""Project the baseline's selected release manifests without changing source manifests."""

import argparse
import json
import tomllib
from pathlib import Path

import tomli_w

parser = argparse.ArgumentParser()
parser.add_argument("--native-jobs", type=int, default=16)
args = parser.parse_args()
if args.native_jobs < 1:
    parser.error("native-jobs must be positive")

root = Path(__file__).resolve().parents[3]
cache = root / ".cache/controller-benchmark"
profile = json.loads((cache / "cargo-features.json").read_text())
workspace = tomllib.loads((root / "Cargo.toml").read_text())
projection = cache / "resolver"
projection.mkdir(parents=True, exist_ok=True)
selected = {value["path"] for value in profile.values()}
manifests = {}
for cargo in sorted((root / "crates").rglob("Cargo.toml")):
    manifest = tomllib.loads(cargo.read_text())
    if "package" not in manifest:
        continue
    directory = cargo.parent.relative_to(root).as_posix()
    package = manifest["package"]
    for key, value in list(package.items()):
        if value == {"workspace": True}:
            package[key] = workspace["workspace"]["package"][key]
    package.pop("workspace", None)
    manifest.pop("dev-dependencies", None)
    manifest.pop("lints", None)
    manifest.pop("test", None)
    for table in [manifest, *manifest.get("target", {}).values()]:
        table.pop("dev-dependencies", None)
        for kind in ["dependencies", "build-dependencies"]:
            for name, dependency in list(table.get(kind, {}).items()):
                if not isinstance(dependency, dict) or not dependency.get("workspace"):
                    continue
                inherited = workspace["workspace"]["dependencies"][name]
                merged = (
                    {"version": inherited}
                    if isinstance(inherited, str)
                    else dict(inherited)
                )
                extra = {
                    key: value
                    for key, value in dependency.items()
                    if key != "workspace"
                }
                extra_features = extra.pop("features", [])
                merged.update(extra)
                if extra_features:
                    merged["features"] = merged.get("features", []) + extra_features
                if "path" in merged:
                    # Workspace-inherited paths are relative to the workspace root.
                    merged["path"] = str(
                        Path("../" * len(Path(directory).parts)) / merged["path"]
                    )
                table[kind][name] = merged
    manifest.setdefault("features", {})["default"] = []
    destination = projection / directory
    destination.mkdir(parents=True, exist_ok=True)
    (destination / "Cargo.toml").write_text(tomli_w.dumps(manifest))
    (destination / "BUILD.bazel").write_text(
        'alias(name = {!r}, actual = {!r}, visibility = ["//visibility:public"])\n'.format(
            destination.name, "//" + directory + ":" + destination.name
        )
    )
    (destination / "src").mkdir(exist_ok=True)
    for entry in [
        manifest.get("lib", {}).get("path", "src/lib.rs"),
        *[b.get("path", "src/main.rs") for b in manifest.get("bin", [])],
    ]:
        target = destination / entry
        target.parent.mkdir(parents=True, exist_ok=True)
        target.touch()
    # Cargo discovers an undeclared default src/main.rs independently of [[bin]].
    if (cargo.parent / "src/main.rs").exists():
        (destination / "src/main.rs").touch()
    manifests[directory] = manifest
projected_workspace = {
    "workspace": {
        "resolver": "3",
        "members": sorted(selected),
        "exclude": sorted(set(manifests) - selected),
    }
}
# Testcontainers adds only test dependencies; the release oracle stays authoritative
# for compiler units shared with the controller image.
test_support = projection / "test-support"
test_support.mkdir(exist_ok=True)
(test_support / "lib.rs").touch()
test_manifest = {
    "package": {
        "name": "controller-benchmark-test-support",
        "version": "0.0.0",
        "edition": "2024",
    },
    "lib": {"path": "lib.rs"},
    "dependencies": {
        name: workspace["workspace"]["dependencies"][name]
        | (
            {"features": ["aws-lc-rs", "postgres"]}
            if name == "testcontainers-modules"
            else {}
        )
        for name in ["testcontainers", "testcontainers-modules"]
    },
}
(test_support / "Cargo.toml").write_text(tomli_w.dumps(test_manifest))
projected_workspace["workspace"]["members"].append("test-support")
(projection / "Cargo.toml").write_text(tomli_w.dumps(projected_workspace))
lock = tomllib.loads((root / "Cargo.lock").read_text())
referenced_names = set(profile)
for directory in selected:
    manifest = manifests[directory]
    for table in [manifest, *manifest.get("target", {}).values()]:
        for kind in ["dependencies", "build-dependencies"]:
            for name, dep in table.get(kind, {}).items():
                if isinstance(dep, dict) and "path" in dep:
                    referenced_names.add(dep.get("package", name))
lock["package"] = [
    p for p in lock["package"] if p.get("source") or p["name"] in referenced_names
]
(projection / "Cargo.lock").write_text(tomli_w.dumps(lock))
lines = []
for name, value in sorted(profile.items()):
    lines.append(
        "crate.annotation(crate = {!r}, crate_features = {!r})".format(
            name, value["features"]
        )
    )
(root / "ci/benchmarks/controller-bazel/features.MODULE.bazel").write_text(
    'crate = use_extension("@rules_rs//rs:extensions.bzl", "crate")\n'
    + "\n".join(lines)
    + "\n"
)
external = {}
for line in (cache / "cargo-profile/benchmark-profile.jsonl").read_text().splitlines():
    artifact = json.loads(line)
    if (
        artifact.get("reason") != "compiler-artifact"
        or artifact["target"]["kind"] == ["custom-build"]
        or artifact["package_id"].startswith("path+")
    ):
        continue
    name, version = artifact["package_id"].rsplit("#", 1)[1].rsplit("@", 1)
    external.setdefault((name, version), set()).update(artifact["features"])
with (root / "ci/benchmarks/controller-bazel/features.MODULE.bazel").open(
    "a"
) as stream:
    for (name, version), enabled in sorted(external.items()):
        stream.write(
            'crate.annotation(crate = {!r}, version = {!r}, build_script_env = {!r}, tags = ["cargo-release-profile"], patches = {!r}, patch_args = {!r})\n'.format(
                name,
                version,
                {"NUM_JOBS": str(args.native_jobs)}
                if name == "tikv-jemalloc-sys"
                else {},
                ["//ci/benchmarks/controller-bazel:patches/sqlite-build-script.patch"]
                if name == "libsqlite3-sys"
                else [],
                ["-p1"] if name == "libsqlite3-sys" else [],
            )
        )
        stream.write(
            f'crate.annotation_select(crate = {name!r}, version = {version!r}, triples = ["x86_64-unknown-linux-gnu"], crate_features = {sorted(enabled)!r})\n'
        )
external_labels = ["@crates//:" + name + "-" + version for name, version in external]
(root / "ci/benchmarks/controller-bazel/closure.bzl").write_text(
    "RELEASE_EXTERNAL = " + repr(sorted(external_labels)) + "\n"
)
features = {value["path"]: value["features"] for value in profile.values()}
(root / "ci/benchmarks/controller-bazel/features.bzl").write_text(
    "RELEASE_FEATURES = " + repr(features) + "\n"
)
print(
    f"Projected {len(selected)} release crates; source Cargo manifests and lockfile unchanged"
)
