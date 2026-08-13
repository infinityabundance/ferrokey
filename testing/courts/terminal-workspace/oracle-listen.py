#!/usr/bin/env python3
"""Listen on the pty-oracle socket and print its JSON lines to stdout.

The terminal-court oracle (testing/targets/pty-oracle.c) connects here when
Ferrokey spawns it as the PTY child; every JSON line it reports is written to
stdout (redirected by the court to $OUT/oracle.log).

Usage: oracle-listen.py <socket-path>
"""

import os
import socket
import sys


def main() -> None:
    path = sys.argv[1]
    try:
        os.unlink(path)
    except FileNotFoundError:
        pass
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(path)
    srv.listen(1)
    conn, _ = srv.accept()
    try:
        while True:
            data = conn.recv(4096)
            if not data:
                break
            sys.stdout.write(data.decode("utf-8", "replace"))
            sys.stdout.flush()
    finally:
        conn.close()
        srv.close()


if __name__ == "__main__":
    main()
