#!/usr/bin/env python3
"""One host-side sample of every Firecracker VM on this Kind node (run inside the node container).

For each `/firecracker --id <id>` process: RSS, cumulative CPU ticks, and a raw
`CONNECT 5002` on the jail's vsock UDS through /proc/<pid>/root/v.sock. An `OK`
reply means the guest kernel and the spaced agent's vsock listener answer,
independent of the host-side spaced bridge/attestation path. Prints one JSON
line per VM.
"""
import json
import os
import socket
import sys
import time


def vsock(path, port, timeout=3.0):
    start = time.monotonic()
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(timeout)
    try:
        sock.connect(path)
        sock.sendall(f"CONNECT {port}\n".encode())
        reply = sock.recv(64).decode(errors="replace").strip()
        return reply.startswith("OK"), reply[:40], round((time.monotonic() - start) * 1e3, 2)
    except Exception as error:  # noqa: BLE001 - recorded as a failed probe
        return False, repr(error)[:80], round((time.monotonic() - start) * 1e3, 2)
    finally:
        sock.close()


def main():
    node = sys.argv[1] if len(sys.argv) > 1 else os.uname().nodename
    ts = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    for pid in filter(str.isdigit, os.listdir("/proc")):
        try:
            argv = open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0")
        except OSError:
            continue
        if not argv or not argv[0].endswith(b"firecracker") or b"--id" not in argv:
            continue
        sbx = argv[argv.index(b"--id") + 1].decode()
        status = dict(line.split(":", 1) for line in open(f"/proc/{pid}/status") if ":" in line)
        stat = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        ok, reply, ms = vsock(f"/proc/{pid}/root/v.sock", 5002)
        print(json.dumps({"ts": ts, "node": node, "sbx": sbx, "pid": int(pid),
                          "rss_kb": int(status["VmRSS"].split()[0]),
                          "cpu_ticks": int(stat[11]) + int(stat[12]),
                          "vsock_ok": ok, "vsock_reply": reply, "vsock_ms": ms}), flush=True)


if __name__ == "__main__":
    main()
