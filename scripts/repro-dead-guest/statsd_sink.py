"""Minimal DogStatsD UDP sink: timestamps and keeps the lifecycle/health series.

Run inside a Kind node's network namespace (the node-stack pod is
hostNetwork and sends to <node-ip>:8125):
  docker run -d --name dd-sink-<node> --network container:<node> \
    -v <out>:/out python:3.12-alpine python -u /out/statsd_sink.py /out/<node>.statsd
"""
import socket
import sys
import time

KEEP = (b"sandbox.guest.health", b"sandbox.health", b"auto_lifecycle",
        b"snapshot.create", b"sandbox.lifecycle")

sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind(("0.0.0.0", 8125))
with open(sys.argv[1], "ab", buffering=0) as out:
    while True:
        data, _ = sock.recvfrom(65535)
        stamp = time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime()).encode()
        for line in data.split(b"\n"):
            if any(key in line for key in KEEP):
                out.write(stamp + b"Z " + line + b"\n")
