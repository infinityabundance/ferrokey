#!/usr/bin/env bash
# IPC.AUTH / IPC.MALFORMED / PERMISSIONS courts (rules 10, 21, 42; §27).
#
# Inside the VM:
#   * an unauthorized user (root is not whitelisted) is rejected via
#     SO_PEERCRED — the kernel-supplied identity, never client input (§27)
#   * the authorized court user may connect and command the keyboard
#   * malformed frames are rejected and the daemon survives
#   * key_up without key_down is rejected
#   * unknown key codes are rejected
#   * flooding is rate-limited
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

SOCK=/run/ferrokeyd/ferrokeyd.sock
VER=2
HELLO=$(python3 -c "import struct; print((bytes([1, $VER]) + struct.pack('<H',5) + b'court').hex())")

start_ferrokeyd

# ── PERMISSIONS.001: unauthorized user rejected ───────────────────────────
# The daemon whitelists uid 1000 only; root (uid 0) must be refused via
# SO_PEERCRED. The client exits 0 when the rejection is observed (EOF with
# no reply, or an ERROR frame).
if sudo -u root python3 - <<EOF 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("$SOCK")
s.settimeout(3)
hello = bytes.fromhex("$HELLO")
s.sendall(b"FK01" + len(hello).to_bytes(2, "little") + hello)
s.sendall(b"FK01" + b"\x01\x00" + b"\x02")
try:
    body = s.recv(4096)
except OSError:
    body = b""
# Unauthorized peers are refused without a reply (EOF) — that IS the rejection.
sys.exit(0 if (not body) or (body[6] == 0x81) else 1)
EOF
then
    ok "unauthorized user (root) rejected"
else
    bad "root was accepted despite not being whitelisted"
fi

# ── IPC.AUTH.001: authorized user accepted ────────────────────────────────
if python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" handshake; then
    ok "authorized user (uid 1000) accepted"
else
    bad "authorized user rejected"
fi

# ── IPC.MALFORMED.001: hostile frames rejected, daemon survives ───────────
if python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" fuzz 40; then
    ok "malformed-frame fuzz: daemon survived"
else
    bad "daemon died or fuzz failed"
fi

# The daemon must still be alive and functional after the fuzz.
if python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" \
        handshake key-down 30 key-up 30 release-all; then
    ok "daemon functional after fuzz"
else
    bad "daemon not functional after fuzz"
    finish_court FAIL "court" "ipc.malformed"
fi

# ── IPC.MALFORMED.002: key_up without key_down ────────────────────────────
if python3 - <<EOF 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("$SOCK")
s.settimeout(3)
def fr(op, payload=b""):
    body = bytes([op]) + payload
    return b"FK01" + len(body).to_bytes(2, "little") + body
s.sendall(fr(1, bytes([$VER]) + b"\x05\x00court"))
s.sendall(fr(2))
s.recv(4096)
s.sendall(fr(0x11, (30).to_bytes(2, "little")))  # key_up without down
body = s.recv(4096)
sys.exit(0 if body and body[6] == 0x81 else 1)
EOF
then
    ok "key_up without key_down rejected"
else
    bad "key_up without key_down accepted"
fi

# ── IPC.MALFORMED.003: unknown key code ───────────────────────────────────
if python3 - <<EOF 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("$SOCK")
s.settimeout(3)
def fr(op, payload=b""):
    body = bytes([op]) + payload
    return b"FK01" + len(body).to_bytes(2, "little") + body
s.sendall(fr(1, bytes([$VER]) + b"\x05\x00court"))
s.sendall(fr(2))
s.recv(4096)
s.sendall(fr(0x10, (0xFFFF).to_bytes(2, "little")))
body = s.recv(4096)
sys.exit(0 if body and body[6] == 0x81 else 1)
EOF
then
    ok "unknown key code rejected"
else
    bad "unknown key code accepted"
fi

# ── IPC.RATE.001: flooding is bounded ─────────────────────────────────────
# The daemon enforces max_connections, and the court's earlier tests tear
# down asynchronously, so a fresh connection can transiently be rejected. A
# rejected connection is dropped without a reply, so we retry until the
# daemon actually accepts: only an accepted connection can prove (or
# disprove) the rate limit.
if python3 - <<EOF 2>/dev/null
import socket, sys, threading, time
sock_path = "$SOCK"
VER = $VER

def fr(op, payload=b""):
    body = bytes([op]) + payload
    return b"FK01" + len(body).to_bytes(2, "little") + body

s = None
for attempt in range(40):
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.settimeout(10)
    try:
        s.connect(sock_path)
        s.sendall(fr(1, bytes([VER]) + b"\x05\x00court"))
        s.sendall(fr(2))
        if s.recv(4096):
            break
    except OSError:
        pass
    try:
        s.close()
    except OSError:
        pass
    time.sleep(0.25)
else:
    sys.exit(1)
# A real hostile client reads replies while flooding: without a concurrent
# reader the socket buffers fill, the daemon's writes back up, and the token
# bucket refills during the stall — masking the limit. With replies drained,
# the daemon processes the flood at full speed and trips the limit at the
# burst boundary.
data = b""
lock = threading.Lock()
def reader():
    global data
    try:
        while True:
            chunk = s.recv(65536)
            if not chunk:
                break
            with lock:
                data += chunk
    except OSError:
        pass
threading.Thread(target=reader, daemon=True).start()
# Blast far more messages than the burst allows. When the daemon hits the
# limit it sends ERROR(RateLimited) and drops the connection — sends after
# that fail, which is expected and fine.
try:
    for i in range(2000):
        s.sendall(fr(0x20, (i % 0xFFFFFFFF).to_bytes(4, "little")))
except OSError:
    pass
time.sleep(0.5)
with lock:
    # The error frame's opcode+code bytes (0x81 0x06 0x00) appear verbatim.
    limited = b"\x81" + (6).to_bytes(2, "little") in data
sys.exit(0 if limited else 1)
EOF
then
    ok "flood was rate-limited"
else
    bad "flood was not rate-limited"
fi

finish_court "court" "ipc"
