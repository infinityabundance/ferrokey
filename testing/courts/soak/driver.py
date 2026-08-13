#!/usr/bin/env python3
"""Soak driver for SEC.SOAK.001 (§98): randomized valid/invalid FK01 traffic.

Interleaves:
  * valid handshake + bounded key events (from the capability set),
  * invalid frames (bad magic, huge lengths, unknown opcodes, bad codes),
  * reconnect churn,
for the given duration, keeping one long-lived session alive so held-key
ownership is exercised continuously. Exits 0 when the broker stays alive and
responds; the caller asserts "driver-complete" in the output.

Runs only inside the disposable VM.
"""
import random
import socket
import struct
import sys
import time

MAGIC = b"FK01"
VER = 2
OP_HELLO = 0x01
OP_OPEN_SESSION = 0x02
OP_DOWN = 0x10
OP_UP = 0x11
OP_RELEASE = 0x12
OP_REPEAT = 0x13
OP_PING = 0x20

# A subset of the explicit capability set (evdev codes) to exercise.
# Deliberately EXCLUDES the Ctrl-Alt-Del trio (14 KEY_BACKSPACE, 29
# KEY_LEFTCTRL, 56 KEY_LEFTALT): the kernel's VT keyboard handler treats
# Ctrl+Alt+Backspace as a reboot request (SIGINT to PID 1 → systemd
# reboot.target), so injecting that combination would reboot the court VM
# under valid traffic. Keyboard injection inherently *can* trigger
# Ctrl-Alt-Del (documented in docs/threat-model.md); the soak just must not
# do it randomly.
KEY_CODES = [30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44,
             45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 57, 58, 59,
             15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28,
             100, 103, 105, 106, 108, 111, 113, 115, 125, 126]


def fr(op: int, payload: bytes = b"") -> bytes:
    body = bytes([op]) + payload
    return MAGIC + struct.pack("<H", len(body)) + body


def invalid_frame(rng: random.Random) -> bytes:
    kind = rng.randrange(8)
    if kind == 0:
        return b""
    if kind == 1:
        return b"JUNK"
    if kind == 2:
        return MAGIC + b"\xff\xff" + bytes(rng.randrange(64))
    if kind == 3:
        return MAGIC + b"\x00\x00"
    if kind == 4:
        return MAGIC + b"\x01\x00" + bytes([rng.randrange(256)])
    if kind == 5:
        return MAGIC + b"\x05\x00" + bytes([rng.randrange(0x20), rng.randrange(256), rng.randrange(256)])
    if kind == 6:
        return bytes(rng.randrange(256) for _ in range(rng.randrange(1, 200)))
    return MAGIC + struct.pack("<H", rng.randrange(0, 100)) + bytes(rng.randrange(256) for _ in range(16))


def main() -> int:
    sock_path = sys.argv[1]
    duration = float(sys.argv[2]) if len(sys.argv) > 2 else 300.0
    seed = int(time.time())
    rng = random.Random(seed)

    deadline = time.monotonic() + duration
    valid = 0
    invalid = 0
    reconnects = 0
    errors = 0

    def connect():
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(2)
        s.connect(sock_path)
        s.sendall(fr(OP_HELLO, bytes([VER]) + struct.pack("<H", 4) + b"soak"))
        s.sendall(fr(OP_OPEN_SESSION))
        if not s.recv(64):
            raise OSError("no handshake reply")
        return s

    sock = connect()
    held: set[int] = set()
    last_ping = time.monotonic()

    while time.monotonic() < deadline:
        # Periodic ping to prove the connection stays healthy.
        if time.monotonic() - last_ping > 10:
            try:
                sock.sendall(fr(OP_PING, struct.pack("<I", 1)))
                if not sock.recv(64):
                    raise OSError("ping EOF")
            except OSError:
                errors += 1
                try:
                    sock.close()
                except OSError:
                    pass
                sock = connect()
                held.clear()
                reconnects += 1
            last_ping = time.monotonic()

        if rng.random() < 0.25:
            # Invalid frame — must be rejected, never reach the kernel path.
            try:
                sock.sendall(invalid_frame(rng))
                invalid += 1
            except OSError:
                errors += 1
                try:
                    sock.close()
                except OSError:
                    pass
                sock = connect()
                held.clear()
                reconnects += 1
            continue

        # Valid key event, keeping held-key discipline.
        code = rng.choice(KEY_CODES)
        if rng.random() < 0.5 and len(held) < 8:
            try:
                sock.sendall(fr(OP_DOWN, struct.pack("<H", code)))
                held.add(code)
                valid += 1
            except OSError:
                errors += 1
                sock = connect()
                held.clear()
                reconnects += 1
        elif code in held:
            try:
                sock.sendall(fr(OP_UP, struct.pack("<H", code)))
                held.discard(code)
                valid += 1
            except OSError:
                errors += 1
                sock = connect()
                held.clear()
                reconnects += 1
        else:
            try:
                sock.sendall(fr(OP_REPEAT, struct.pack("<H", code)))
                valid += 1
            except OSError:
                errors += 1
                sock = connect()
                held.clear()
                reconnects += 1

        # Draining: keep the daemon's output queue empty.
        try:
            sock.settimeout(0)
            while sock.recv(65536):
                pass
        except (OSError, BlockingIOError):
            pass
        sock.settimeout(2)

        if rng.random() < 0.005:
            # Reconnect churn.
            try:
                sock.close()
            except OSError:
                pass
            sock = connect()
            held.clear()
            reconnects += 1

    # Final release-all must succeed: the broker's ledger must be clean.
    try:
        sock.sendall(fr(OP_RELEASE))
        if sock.recv(64):
            pass
    except OSError:
        errors += 1
    try:
        sock.close()
    except OSError:
        pass

    print(f"driver-complete valid={valid} invalid={invalid} "
          f"reconnects={reconnects} errors={errors} held={len(held)}")
    return 0 if errors < reconnects + 5 else 1


if __name__ == "__main__":
    sys.exit(main())
