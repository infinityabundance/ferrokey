#!/usr/bin/env bash
# IPC.AUTH / IPC.MALFORMED / PERMISSIONS courts (rules 10, 21, 42).
#
# Inside the VM:
#   * an unauthorized user (root is not whitelisted) is rejected
#   * the authorized court user may connect and command the keyboard
#   * malformed frames are rejected and the daemon survives
#   * key_up without key_down is rejected
#   * unknown key codes are rejected
#   * flooding is rate-limited
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

start_ferrokeyd

# ── PERMISSIONS.001: unauthorized user rejected ───────────────────────────
# The daemon whitelists uid 1000 only; root (uid 0) must be refused.
if sudo -u root python3 - <<'EOF' 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("/tmp/ferrokeyd.sock")
s.settimeout(3)
s.sendall(b"FK01" + b"\x05\x00" + b"\x01\x01\x04\x00court")
s.sendall(b"FK01" + b"\x01\x00" + b"\x02")
try:
    body = s.recv(4096)
except OSError:
    body = b""
# Unauthorized peers are refused without a reply (EOF) — that IS the rejection.
sys.exit(0 if (not body) or (body[6] == 0x81) else 1)
EOF
then
    bad "root was accepted despite not being whitelisted"
else
    ok "unauthorized user (root) rejected"
fi

# ── IPC.AUTH.001: authorized user accepted ────────────────────────────────
if python3 "$PAYLOAD/courts/fk-client.py" --socket /tmp/ferrokeyd.sock handshake; then
    ok "authorized user (uid 1000) accepted"
else
    bad "authorized user rejected"
fi

# ── IPC.MALFORMED.001: hostile frames rejected, daemon survives ───────────
if python3 "$PAYLOAD/courts/fk-client.py" --socket /tmp/ferrokeyd.sock fuzz 40; then
    ok "malformed-frame fuzz: daemon survived"
else
    bad "daemon died or fuzz failed"
fi

# The daemon must still be alive and functional after the fuzz.
if python3 "$PAYLOAD/courts/fk-client.py" --socket /tmp/ferrokeyd.sock \
        handshake key-down 30 key-up 30 release-all; then
    ok "daemon functional after fuzz"
else
    bad "daemon not functional after fuzz"
    finish_court FAIL "court" "ipc.malformed"
fi

# ── IPC.MALFORMED.002: key_up without key_down ────────────────────────────
if python3 - <<'EOF' 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("/tmp/ferrokeyd.sock")
s.settimeout(3)
def fr(op, payload=b""):
    body = bytes([op]) + payload
    return b"FK01" + len(body).to_bytes(2, "little") + body
s.sendall(fr(1, b"\x01\x04\x00court"))
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
if python3 - <<'EOF' 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("/tmp/ferrokeyd.sock")
s.settimeout(3)
def fr(op, payload=b""):
    body = bytes([op]) + payload
    return b"FK01" + len(body).to_bytes(2, "little") + body
s.sendall(fr(1, b"\x01\x04\x00court"))
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
if python3 - <<'EOF' 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("/tmp/ferrokeyd.sock")
s.settimeout(3)
def fr(op, payload=b""):
    body = bytes([op]) + payload
    return b"FK01" + len(body).to_bytes(2, "little") + body
s.sendall(fr(1, b"\x01\x04\x00court"))
s.sendall(fr(2))
s.recv(4096)
# Blast far more messages than the burst allows.
for i in range(2000):
    s.sendall(fr(0x20, (i % 0xFFFFFFFF).to_bytes(4, "little")))
limited = False
try:
    while True:
        body = s.recv(4096)
        if not body:
            break
        if body[6] == 0x81 and body[7:9] == (6).to_bytes(2, "little"):
            limited = True
            break
except socket.timeout:
    pass
sys.exit(0 if limited else 1)
EOF
then
    ok "flood was rate-limited"
else
    bad "flood was not rate-limited"
fi

finish_court PASS "court" "ipc"
