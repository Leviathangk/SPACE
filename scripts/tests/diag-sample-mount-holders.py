#!/usr/bin/env python3
"""Read-only /proc sampler. Run as root in the Kind node's PID namespace."""
import argparse
import glob
import json
import os
import time

NEEDLE = "space-oci-rootfs-"


def read(path):
    try:
        with open(path) as stream:
            return stream.read()
    except (OSError, UnicodeError):
        return ""


def link(path):
    try:
        return os.readlink(path)
    except OSError:
        return ""


def snapshot():
    records = []
    namespaces = {}
    processes = []
    # Discover mount IDs separately from path aliases. An fd may name the
    # containerd snapshot source rather than the bind-mount destination.
    for proc in glob.glob("/proc/[0-9]*"):
        ns = link(proc + "/ns/mnt")
        if not ns:
            continue
        pid = int(proc.rsplit("/", 1)[1])
        comm = read(proc + "/comm").strip()
        processes.append((proc, pid, comm, ns))
        if ns not in namespaces:
            lines = read(proc + "/mountinfo").splitlines()
            matches = [line for line in lines if NEEDLE in line]
            namespaces[ns] = {
                "ids": {line.split()[0] for line in matches},
                "matches": matches,
                "cache": [
                    line for line in lines
                    if line.split()[4] in ("/space/cache", "/space/cache/firecracker")
                ],
            }
    for proc, pid, comm, ns in processes:
        info = namespaces[ns]
        base = {"pid": pid, "comm": comm, "mnt_ns": ns}
        if info["matches"]:
            records.append({
                **base, "type": "mountinfo", "lines": info["matches"],
                "cache_mountinfo": info["cache"],
            })
        for kind in ("cwd", "root", "exe"):
            target = link(proc + "/" + kind)
            if NEEDLE in target:
                records.append({**base, "type": kind, "target": target})
        for fd in glob.glob(proc + "/fd/*"):
            target = link(fd)
            fdinfo = read(proc + "/fdinfo/" + fd.rsplit("/", 1)[1])
            mnt_id = next(
                (line.split()[1] for line in fdinfo.splitlines()
                 if line.startswith("mnt_id:")), None
            )
            if NEEDLE in target or mnt_id in info["ids"]:
                records.append({
                    **base, "type": "fd", "fd": fd, "target": target,
                    "fdinfo": fdinfo,
                })
        maps = [line for line in read(proc + "/maps").splitlines() if NEEDLE in line]
        if maps:
            records.append({**base, "type": "maps", "lines": maps})
    return records


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interval", type=float, default=0.05)
    parser.add_argument("--duration", type=float, default=0)
    args = parser.parse_args()
    started = time.monotonic()
    while not args.duration or time.monotonic() - started < args.duration:
        tick = time.monotonic()
        records = snapshot()
        elapsed = time.monotonic() - tick
        # Heartbeats prove observation coverage even when there are no mounts.
        print(json.dumps({
            "wall_time_ns": time.time_ns(), "scan_seconds": elapsed,
            "records": records,
        }), flush=True)
        time.sleep(max(0, args.interval - elapsed))


if __name__ == "__main__":
    main()
