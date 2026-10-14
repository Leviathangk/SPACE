#!/usr/bin/env python3
"""Check the image contract before interpreting build-time comparisons."""

import hashlib
import json
import subprocess
import tarfile
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
CACHE = ROOT / ".cache/controller-benchmark"
BINARY = "/space/bin/space-cluster-controller"
IMAGES = {
    "cargo": "space-controller-cargo:benchmark",
    "bazel": "space-controller-bazel:benchmark",
}
TRANSIENT = {"etc/hostname", "etc/hosts", "etc/resolv.conf", ".dockerenv"}


def output(command):
    return subprocess.check_output(command, text=True)


def filesystem(image, backend):
    name = "controller-benchmark-" + uuid.uuid4().hex[:12]
    subprocess.run(
        ["docker", "create", "--network", "none", "--name", name, image],
        check=True,
        stdout=subprocess.DEVNULL,
    )
    archive = CACHE / f"{backend}-rootfs.tar"
    try:
        with archive.open("wb") as stream:
            subprocess.run(["docker", "export", name], stdout=stream, check=True)
    finally:
        subprocess.run(["docker", "rm", name], check=True, stdout=subprocess.DEVNULL)
    entries = {}
    binary_type = None
    with tarfile.open(archive) as source:
        for member in source:
            path = member.name.removeprefix("./").rstrip("/")
            if path in TRANSIENT or not path:
                continue
            digest = None
            if member.isfile():
                content = source.extractfile(member).read()
                digest = hashlib.sha256(content).hexdigest()
                if path == BINARY.removeprefix("/"):
                    assert content[:4] == b"\x7fELF", (
                        "controller is not an ELF executable"
                    )
                    binary_type = int.from_bytes(content[16:18], "little")
                    digest = "controller-binary"
            entries[path] = [
                member.type.decode(),
                member.mode,
                member.uid,
                member.gid,
                member.linkname,
                digest,
            ]
    return entries, binary_type


def main():
    configs = {}
    cli = {}
    files = {}
    elf = {}
    for backend, image in IMAGES.items():
        config = json.loads(output(["docker", "image", "inspect", image]))[0]["Config"]
        configs[backend] = {
            key: config.get(key)
            for key in ["User", "WorkingDir", "Entrypoint", "Cmd", "Env", "Labels"]
        }
        cli[backend] = {
            flag: output(
                ["docker", "run", "--rm", "--network", "none", image, BINARY, flag]
            )
            for flag in ["--version", "--help"]
        }
        files[backend], elf[backend] = filesystem(image, backend)
    file_differences = {
        key: {backend: entries.get(key) for backend, entries in files.items()}
        for key in set(files["cargo"]) | set(files["bazel"])
        if files["cargo"].get(key) != files["bazel"].get(key)
    }
    report = {
        "config_equal": configs["cargo"] == configs["bazel"],
        "cli_equal": cli["cargo"] == cli["bazel"],
        "elf_types": elf,
        "filesystem_differences": file_differences,
        "configs": configs,
        "version": cli["cargo"]["--version"].strip(),
    }
    (CACHE / "image-parity.json").write_text(json.dumps(report, indent=2) + "\n")
    print(
        json.dumps(
            {key: value for key, value in report.items() if key != "configs"}, indent=2
        )
    )
    if (
        not report["config_equal"]
        or not report["cli_equal"]
        or elf["cargo"] != elf["bazel"]
        or file_differences
    ):
        raise SystemExit("Image parity failed; do not compare timings yet")


if __name__ == "__main__":
    main()
