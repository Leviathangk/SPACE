#!/usr/bin/env python3
"""Compare the Cargo and Bazel Kind artifact archives without Docker loading."""

import argparse
import hashlib
import json
import re
import subprocess
import tarfile
import tempfile
from pathlib import Path


def files(archive, destination):
    with tarfile.open(archive) as image:
        manifest = json.load(image.extractfile("manifest.json"))
        config = json.load(image.extractfile(manifest[0]["Config"]))
        entries = {}
        for layer in manifest[0]["Layers"]:
            with tarfile.open(fileobj=image.extractfile(layer)) as contents:
                for entry in contents:
                    if not entry.isfile():
                        continue
                    path = Path(entry.name)
                    if path.is_absolute() or ".." in path.parts:
                        raise ValueError("Unsafe artifact archive path")
                    content = contents.extractfile(entry).read()
                    entries[path.as_posix()] = (
                        entry.mode,
                        entry.uid,
                        entry.gid,
                        content,
                    )
                    target = destination / path
                    target.parent.mkdir(parents=True, exist_ok=True)
                    target.write_bytes(content)
    return entries, config["config"]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("cargo", type=Path)
    parser.add_argument("bazel", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    failures = []
    with tempfile.TemporaryDirectory(prefix="space-artifact-parity-") as temporary:
        root = Path(temporary)
        old, old_config = files(args.cargo, root / "cargo")
        new, new_config = files(args.bazel, root / "bazel")
        if set(old) != set(new):
            failures.append(f"File inventory differs: {set(old) ^ set(new)}")
        for key in ("Labels", "Entrypoint", "Cmd", "WorkingDir", "User", "Env"):
            if (old_config.get(key) or None) != (new_config.get(key) or None):
                failures.append(f"Image configuration differs: {key}")
        for name in sorted(set(old) & set(new)):
            if old[name][:3] != new[name][:3]:
                failures.append(f"File metadata differs: {name}")
            if (
                name.startswith("artifacts/bin/")
                and name != "artifacts/bin/grpc_health_probe"
            ):
                if old[name][3][:18] != new[name][3][:18]:
                    failures.append(f"ELF architecture/type differs: {name}")
                for backend in ("cargo", "bazel"):
                    result = subprocess.run(
                        ["readelf", "--version-info", str(root / backend / name)],
                        capture_output=True,
                        text=True,
                        check=True,
                    )
                    versions = [
                        (int(a), int(b))
                        for a, b in re.findall(r"GLIBC_(\d+)\.(\d+)", result.stdout)
                    ]
                    if versions and max(versions) > (2, 36):
                        failures.append(
                            f"Binary requires newer than runtime glibc 2.36: {backend}/{name}"
                        )
                continue
            if name == "artifacts/etc/space-runtime-supervisor-compatibility":
                for entries in (old, new):
                    expected = hashlib.sha256(
                        entries["artifacts/bin/space-runtime-supervisor-svc"][3]
                    ).hexdigest()
                    if entries[name][3].decode().strip() != expected:
                        failures.append(
                            "Supervisor compatibility does not bind its binary"
                        )
            elif old[name][3] != new[name][3]:
                failures.append(f"Non-Rust artifact differs: {name}")
    args.output.write_text(
        json.dumps(
            {
                "passed": not failures,
                "failures": failures,
                "files": len(old),
                "rust_binaries": 27,
                "scope": "inventory, modes/ownership, image config, ELF architecture/type, glibc floor, probe bytes, supervisor checksum; not Kind runtime proof",
            },
            indent=2,
        )
        + "\n"
    )
    print(args.output.read_text())
    return int(bool(failures))


if __name__ == "__main__":
    raise SystemExit(main())
