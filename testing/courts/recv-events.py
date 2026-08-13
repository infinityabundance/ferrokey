#!/usr/bin/env python3
"""Connect to a target application's reporter socket and echo JSON lines.

The court asserts against these machine-readable events (rule 17); screenshots
are never the oracle.
"""
import json
import socket
import sys
import time

path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/ferrokey-test-target.sock"

sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
# Targets bind the socket only once their app is ready (Electron's cold start
# can exceed 10s), and every target replays a state snapshot to new clients —
# so a patient retry is safe and never loses early events.
for _ in range(600):
    try:
        sock.connect(path)
        break
    except (FileNotFoundError, ConnectionRefusedError):
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
            json.loads(line)
        except json.JSONDecodeError:
            continue
        # A wall-clock prefix keeps every line self-timestamped; the courts'
        # greps match the JSON payload regardless of the prefix.
        print(f"{time.time():.3f} {line.decode()}", flush=True)
