#!/usr/bin/env python3
"""Extract the independent Cargo compiler-artifact oracle for this revision."""

import json
from pathlib import Path

root = Path(__file__).resolve().parents[3]
cache = root / ".cache/controller-benchmark"
metadata = json.loads((cache / "cargo-profile/benchmark-metadata.json").read_text())
packages = {p["id"]: p for p in metadata["packages"]}
features = {}
units = {}
supervisor_units = {}
supervisor_paths = set()
secondary = False
host = set()
target = set()
macros = set()
for line in (cache / "cargo-profile/benchmark-profile.jsonl").read_text().splitlines():
    artifact = json.loads(line)
    if artifact.get("reason") == "build-finished":
        secondary = True
        continue
    if artifact.get("reason") != "compiler-artifact" or artifact["target"]["kind"] == [
        "custom-build"
    ]:
        continue
    name, version = artifact["package_id"].split("#")[-1].rsplit("@", 1)
    key = name + "-" + version
    if artifact["target"]["kind"] == ["proc-macro"]:
        macros.add(key)
    role = "host" if artifact["profile"]["opt_level"] == "0" else "target"
    unit = (supervisor_units if secondary else units).setdefault(key, {})
    enabled = sorted(artifact["features"])
    if role in unit and unit[role] != enabled:
        raise SystemExit(
            f"multiple distinct {role} profiles for {key}; extend the projection first"
        )
    unit[role] = enabled
    (host if role == "host" else target).add(key)
    if artifact["package_id"].startswith("path+"):
        package = packages[artifact["package_id"]]
        path = str(Path(package["manifest_path"]).parent).removeprefix("/workspace/")
        prior = features.get(name)
        if secondary:
            if prior is None or prior["features"] != enabled:
                supervisor_paths.add(path)
        elif prior and prior["features"] != enabled:
            raise SystemExit(f"multiple first-party profiles for {name}")
        if prior is None or not secondary:
            features[name] = {"features": enabled, "path": path, "version": version}
for key, unit in supervisor_units.items():
    units.setdefault(key, unit)

(cache / "cargo-features.json").write_text(json.dumps(features, indent=2) + "\n")
(root / "ci/benchmarks/controller-bazel/units.bzl").write_text(
    "CARGO_UNITS = "
    + repr(units)
    + "\nHOST_PACKAGES = "
    + repr(sorted(host))
    + "\nTARGET_PACKAGES = "
    + repr(sorted(target))
    + "\nSUPERVISOR_UNITS = "
    + repr(supervisor_units)
    + "\nSUPERVISOR_VARIANT_PATHS = "
    + repr(sorted(supervisor_paths))
    + "\nPROC_MACROS = "
    + repr(sorted(macros))
    + "\n"
)
print(
    f"Captured {len(features)} workspace packages and {len(units)} Cargo release packages"
)
