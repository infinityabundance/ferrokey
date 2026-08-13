#!/usr/bin/env bash
# SEC.KERNEL.* — the KASAN/UBSAN/LOCKDEP debug-kernel court (§66–§68).
#
# Runs ONLY inside a disposable VM booted with the instrumented kernel
# (build-kasan-kernel.sh + KASAN=1) — never on the developer host (§55).
# It exercises the complete Ferrokey-exposed uinput path under an
# instrumented kernel and requires the guest kernel log to stay clean:
# any KASAN/UBSAN/LOCKDEP report, BUG/WARNING/Oops/panic is an automatic
# failure (§67, §68).
#
# Gates:
#   SEC.KERNEL.KASAN       the booted kernel is genuinely instrumented
#                          (CONFIG_KASAN/UBSAN/PROVE_LOCKING from
#                          /proc/config.gz — observation, not assumption)
#   SEC.KERNEL.KASAN_CLEAN the kernel log is clean after the hostile path
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

SOCK=/run/ferrokeyd/ferrokeyd.sock

# ── the kernel must really be instrumented (§66) ────────────────────────────
CONFIG="$OUT/kernel-config.txt"
if [ -f /proc/config.gz ]; then
    zcat /proc/config.gz > "$CONFIG" 2>/dev/null || true
fi
if [ -f "$CONFIG" ] && grep -q "CONFIG_KASAN=y" "$CONFIG" \
    && grep -q "CONFIG_UBSAN=y" "$CONFIG" \
    && grep -q "CONFIG_PROVE_LOCKING=y" "$CONFIG"; then
    ok "SEC.KERNEL.KASAN instrumented kernel confirmed (KASAN+UBSAN+LOCKDEP)"
else
    bad "SEC.KERNEL.KASAN instrumented kernel NOT confirmed (missing CONFIG flags)"
fi
uname -a

# ── exercise the Ferrokey-exposed uinput path under the instrumented kernel ─
start_ferrokeyd

# Valid events: handshake + keys + release-all.
python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" \
    handshake key-down 30 key-up 30 key-down 42 key-down 43 release-all || true
# Hostile protocol fuzz (§53): every decoder corner.
python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" fuzz 100 || true
# Malformed/oversized raw frames.
python3 - "$SOCK" <<'EOF' || true
import socket, struct, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(0.5)
try:
    s.connect(sys.argv[1])
    for f in [
        b"FK01" + (0xFFFF).to_bytes(2, "little") + b"\x00" * 4096,
        b"FK01" + b"\x05\x00" + b"\x10\xff\xff",
        b"XXXX" + (10).to_bytes(2, "little") + b"\x00" * 10,
        b"\x00" * 8192,
    ]:
        try:
            s.sendall(f)
        except OSError:
            break
except OSError:
    pass
EOF

# Reconnect churn: logical sessions must never create kernel devices (§64).
python3 - "$SOCK" <<'EOF' || true
import socket, struct, sys, time
sock_path = sys.argv[1]
def fr(op, payload=b""):
    body = bytes([op]) + payload
    return b"FK01" + struct.pack("<H", len(body)) + body
for i in range(200):
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(0.5)
        s.connect(sock_path)
        s.sendall(fr(1, bytes([2]) + b"\x05\x00storm"))
        s.sendall(fr(2))
        s.recv(64)
    except OSError:
        pass
    try:
        s.close()
    except OSError:
        pass
time.sleep(0.5)
EOF

# Abrupt disconnect: the broker must release exactly that session's keys
# (§22, §74), observed at the kernel level via evtest.
EVENT_NODE=$(ferrokey_device_node)
if command -v evtest >/dev/null 2>&1 && [ -n "$EVENT_NODE" ]; then
    : > "$OUT/kasan-release.log"
    ( timeout 12 sudo -u root evtest --grab "/dev/input/$EVENT_NODE" > "$OUT/kasan-release.log" 2>&1 || true ) &
    EVTEST_PID=$!
    sleep 1
    python3 - "$SOCK" <<'EOF' || true
import socket, struct, sys, os, time
sock_path = sys.argv[1]
def fr(op, payload=b""):
    body = bytes([op]) + payload
    return b"FK01" + struct.pack("<H", len(body)) + body
s = None
for attempt in range(40):
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(2)
        s.connect(sock_path)
        s.sendall(fr(1, bytes([2]) + b"\x04\x00hold"))
        s.sendall(fr(2))
        if not s.recv(64):
            raise OSError("no handshake reply")
        break
    except OSError:
        try:
            s.close()
        except OSError:
            pass
        time.sleep(0.25)
else:
    os._exit(1)
s.sendall(fr(0x10, (30).to_bytes(2, "little")))   # KEY_A down
time.sleep(0.5)
os.kill(os.getpid(), 9)
EOF
    sleep 2
    kill "$EVTEST_PID" 2>/dev/null || true
    wait "$EVTEST_PID" 2>/dev/null || true
    if grep -Eq "KEY_A.*value 1" "$OUT/kasan-release.log" \
        && grep -Eq "KEY_A.*value 0" "$OUT/kasan-release.log"; then
        ok "SEC.KERNEL.KASAN abrupt-disconnect release observed on instrumented kernel"
    else
        bad "SEC.KERNEL.KASAN key release not observed"
    fi
fi

# SIGKILL the broker: the device must unregister, no privilege residue (§74).
SERVE_PID=$(ferrokeyd_serve_pid)
if [ -n "$SERVE_PID" ]; then
    sudo kill -9 "$SERVE_PID" 2>/dev/null || true
    sleep 2
    if [ -z "$(ferrokeyd_serve_pid)" ]; then
        ok "SEC.KERNEL.KASAN broker SIGKILL cleanup clean"
    else
        bad "SEC.KERNEL.KASAN broker SIGKILL left residue"
    fi
fi

# ── THE GATE: the instrumented kernel must stay clean (§67, §68) ────────────
# The regex matches actual diagnostic reports only — bare 'panic'/'lockdep'
# would false-positive on the boot cmdline ('panic=-1') and on the normal
# 'RCU lockdep checking is enabled' announcement.
if sudo -u root dmesg > "$OUT/kernel-debug-dmesg.txt" 2>&1; then
    if grep -qE "BUG:|WARNING:|Oops:|Kernel panic|general protection fault|use-after-free|KASAN:|UBSAN:|kernel BUG|out of bounds|possible circular locking" "$OUT/kernel-debug-dmesg.txt"; then
        bad "SEC.KERNEL.KASAN_CLEAN kernel diagnostics under instrumented kernel"
        grep -E "BUG:|WARNING:|Oops:|KASAN:|UBSAN:|possible circular locking|general protection fault|Kernel panic" "$OUT/kernel-debug-dmesg.txt" | head -8
    else
        ok "SEC.KERNEL.KASAN_CLEAN kernel log clean (KASAN+UBSAN+LOCKDEP active)"
    fi
else
    echo "dmesg restricted; SEC.KERNEL.KASAN_CLEAN skipped (SKIP != PASS, §95)"
    bad "SEC.KERNEL.KASAN_CLEAN dmesg unavailable (SKIP)"
fi

finish_court "court" "kernel-debug"
