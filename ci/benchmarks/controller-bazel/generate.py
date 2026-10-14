#!/usr/bin/env python3
"""Generate experimental first-party rules from the workspace's Cargo manifests."""

import json
import tomllib
from pathlib import Path

root = Path(__file__).resolve().parents[3]
workspace = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]
selected = {
    v["path"]
    for v in json.loads(
        (root / ".cache/controller-benchmark/cargo-features.json").read_text()
    ).values()
}
for cargo in sorted((root / "crates").rglob("Cargo.toml")):
    directory = cargo.parent
    if directory.relative_to(root).as_posix() not in selected:
        existing = directory / "BUILD.bazel"
        if existing.exists() and existing.read_text().startswith(
            'load("//ci/benchmarks/controller-bazel:crates.bzl"'
        ):
            existing.unlink()
        continue
    if (directory / "BUILD.bazel").exists() and "# gazelle:lang rs" in (
        directory / "BUILD.bazel"
    ).read_text():
        continue
    manifest = tomllib.loads((directory / "Cargo.toml").read_text())
    package = manifest["package"]
    version = package["version"]
    if isinstance(version, dict):
        version = workspace["package"]["version"]
    library = directory / manifest.get("lib", {}).get("path", "src/lib.rs")
    binary = not library.is_file()
    if binary:
        binaries = manifest.get("bin", [])
        if len(binaries) != 1:
            continue
        entry = binaries[0].get("path", "src/main.rs")
    else:
        entry = library.relative_to(directory).as_posix()
    destination = directory / "BUILD.bazel"
    if destination.exists() and not destination.read_text().startswith(
        'load("//ci/benchmarks/controller-bazel:crates.bzl"'
    ):
        raise SystemExit(f"refusing to overwrite {destination}")
    attributes = dict(package)
    for key, value in list(attributes.items()):
        if value == {"workspace": True}:
            attributes[key] = workspace["package"][key]
    rust_name = (
        binaries[0]["name"]
        if binary
        else manifest.get("lib", {}).get("name", package["name"])
    ).replace("-", "_")
    cargo_env = {
        "CARGO_PKG_NAME": package["name"],
        "CARGO_PKG_VERSION": version,
        "CARGO_PKG_AUTHORS": ":".join(attributes.get("authors", [])),
        "CARGO_PKG_DESCRIPTION": attributes.get("description", ""),
        "CARGO_PKG_HOMEPAGE": attributes.get("homepage", ""),
        "CARGO_PKG_REPOSITORY": attributes.get("repository", ""),
        "CARGO_PKG_LICENSE": attributes.get("license", ""),
        "CARGO_PKG_LICENSE_FILE": attributes.get("license-file", ""),
        "CARGO_PKG_RUST_VERSION": attributes.get("rust-version", ""),
    }
    if binary:
        cargo_env["CARGO_BIN_NAME"] = binaries[0]["name"]
    arguments = {
        "name": directory.name,
        "cargo_name": package["name"],
        "rust_name": rust_name,
        "cargo_env": cargo_env,
        "version": version,
        "crate_root": entry,
        "binary": binary,
        "proc_macro": manifest.get("lib", {}).get("proc-macro", False),
        "build_script": (directory / "build.rs").is_file(),
    }
    content = 'load("//ci/benchmarks/controller-bazel:crates.bzl", "space_crate")\n\nspace_crate(\n'
    content += "".join(f"    {key} = {value!r},\n" for key, value in arguments.items())
    destination.write_text(content + ")\n")
