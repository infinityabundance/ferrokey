#!/usr/bin/env python3
"""Minimal FK01 protocol client for the compatibility courts.

Usage:
  fk-client.py --socket PATH handshake [key-down CODE] [key-up CODE]
               [release-all] [ping NONCE] ...
  fk-client.py --socket PATH fuzz N          # send N malformed frames
  fk-client.py --socket PATH drain           # read replies until EOF

Exits 0 if the daemon behaved as expected for the given sequence.
"""
import socket
import struct
import sys
import time

MAGIC = b"FK01"
OP_HELLO = 0x01
OP_CREATE = 0x02
OP_DOWN = 0x10
OP_UP = 0x11
OP_RELEASE = 0x12
OP_PING = 0x20
OP_PONG = 0x21
OP_OK = 0x80
OP_ERROR = 0x81


def frame(opcode: int, payload: bytes = b"") -> bytes:
    body = bytes([opcode]) + payload
    return MAGIC + struct.pack("<H", len(body)) + body


def read_frame(sock: socket.socket):
    header = b""
    while len(header) < 6:
        chunk = sock.recv(6 - len(header))
        if not chunk:
            return None
        header += chunk
    if header[:4] != MAGIC:
        return ("bad-magic", header)
    (length,) = struct.unpack("<H", header[4:6])
    body = b""
    while len(body) < length:
        chunk = sock.recv(length - len(body))
        if not chunk:
            return None
        body += chunk
    return body


def main() -> int:
    args = sys.argv[1:]
    sock_path = "/tmp/ferrokeyd.sock"
    if "--socket" in args:
        i = args.index("--socket")
        sock_path = args[i + 1]
        args = args[:i] + args[i + 2:]

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect(sock_path)
    sock.settimeout(3)

    mode = args[0] if args else "handshake"

    if mode == "fuzz":
        n = int(args[1]) if len(args) > 1 else 50
        patterns = [
            b"",
            b"JUNK",
            MAGIC + b"\xff\xff" + b"x" * 100,
            MAGIC + b"\x05\x00" + b"\x7f" + b"\x00" * 4,
            MAGIC + b"\x00\x00",
            MAGIC + b"\x01\x00" + b"\x10" + b"\x00",
            b"\x00" * 64,
            MAGIC + b"\xff\x7f" + b"\x00" * 5000,
        ]
        for i in range(n):
            data = patterns[i % len(patterns)]
            try:
                sock.sendall(data)
                # Drain whatever the daemon replies, then reconnect.
                sock.settimeout(0.2)
                try:
                    while read_frame(sock):
                        pass
                except socket.timeout:
                    pass
            except OSError:
                pass
            # The daemon must survive: reconnect for the next pattern.
            try:
                sock.close()
            except OSError:
                pass
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.connect(sock_path)
            sock.settimeout(0.2)
            # Re-handshake after each reconnect.
            sock.sendall(frame(OP_HELLO, bytes([1]) + struct.pack("<H", 4) + b"fuzz"))
            sock.sendall(frame(OP_CREATE))
            time.sleep(0.05)
        print("fuzz: daemon survived")
        return 0

    if mode == "drain":
        sock.settimeout(2)
        count = 0
        while True:
            body = read_frame(sock)
            if body is None:
                break
            count += 1
            if isinstance(body, tuple):
                print(f"reply: {body}")
            else:
                print(f"reply: opcode={body[0]:#x}")
        print(f"drain: {count} replies")
        return 0

    # handshake + command sequence
    error = False
    for cmd in args:
        parts = cmd.split()
        op = parts[0]
        if op == "handshake":
            sock.sendall(frame(OP_HELLO, bytes([1]) + struct.pack("<H", 4) + b"court"))
            sock.sendall(frame(OP_CREATE))
            body = read_frame(sock)
            if body and body[0] == OP_OK:
                print("handshake: ok")
            else:
                print("handshake: FAILED", body)
                error = True
        elif op == "key-down":
            sock.sendall(frame(OP_DOWN, struct.pack("<H", int(parts[1]))))
        elif op == "key-up":
            sock.sendall(frame(OP_UP, struct.pack("<H", int(parts[1]))))
        elif op == "release-all":
            sock.sendall(frame(OP_RELEASE))
            body = read_frame(sock)
            if body and body[0] == OP_OK:
                print("release-all: ok")
            else:
                print("release-all: FAILED", body)
                error = True
        elif op == "ping":
            nonce = int(parts[1]) if len(parts) > 1 else 1
            sock.sendall(frame(OP_PING, struct.pack("<I", nonce)))
            body = read_frame(sock)
            if body and body[0] == OP_PONG and struct.unpack("<I", body[1:5])[0] == nonce:
                print(f"ping: ok ({nonce})")
            else:
                print("ping: FAILED", body)
                error = True
        elif op == "expect-error":
            # Send an invalid message and expect an Error reply + teardown.
            sock.sendall(frame(OP_DOWN, struct.pack("<H", 0xFFFF)))
            body = read_frame(sock)
            if body and body[0] == OP_ERROR:
                print("expect-error: ok (rejected)")
            else:
                print("expect-error: FAILED", body)
                error = True
        elif op == "no-hello":
            # Send CREATE_KEYBOARD without HELLO: must be rejected.
            sock.sendall(frame(OP_CREATE))
            body = read_frame(sock)
            if body and body[0] == OP_ERROR:
                print("no-hello: ok (rejected)")
            else:
                print("no-hello: FAILED", body)
                error = True
        else:
            print(f"unknown command: {op}")
            error = True
    return 1 if error else 0


if __name__ == "__main__":
    sys.exit(main())
