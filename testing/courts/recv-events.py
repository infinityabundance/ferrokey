#!/usr/bin/env python3
"""Connect to a target application's reporter socket and echo JSON lines.

The court asserts against these machine-readable events (rule 17); screenshots
are never the oracle.
"""
import json
import socket
import sys

path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/ferrokey-test-target.sock"

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
for _ in range(50):
    try:
        sock.connect(path)
        break
    except (FileNotFoundError, ConnectionRefusedError):
        import time

        time.sleep(0.2)
else:
    print("could not connect to reporter socket", file=sys.stderr)
    sys.exit(1)

buf = b""
while True:
    data = sock.recv(65536)
    if not data:
        break
    buf += data
    while b"\n" in buf:
        line, buf = buf.split(b"\n", 1)
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        print(line.decode(), flush=True)
