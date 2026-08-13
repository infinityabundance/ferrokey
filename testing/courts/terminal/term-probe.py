#!/usr/bin/env python3
"""Ferrokey terminal-court probe: captures raw terminal input.

Runs inside `xterm` (the terminal court's target). It puts the PTY into raw
mode and writes every byte it receives to a log file as space-separated hex —
a *continuous* stream with one token per byte and an explicit flush per read,
so key sequences match the court's `grep` assertions no matter how the PTY
chunks the bytes:

  "abc"         -> 61 62 63
  Left arrow    -> 1b 5b 44        (ESC [ D)
  Up arrow      -> 1b 5b 41        (ESC [ A)
  Home          -> 1b 5b 48        (ESC [ H)
  F5            -> 1b 5b 31 35 7e  (ESC [ 1 5 ~)
  Ctrl+C        -> 03
  Ctrl+D        -> 04
  Ctrl+L        -> 0c

(A per-read newline format would split a sequence like 'abc' across lines
whenever the keys arrive in separate reads; the court asserts whole sequences
like "61 62 63", so the stream must not introduce line breaks between bytes.)

The probe writes `ready` on its own line once it holds the PTY in raw mode.

Raw mode is the honest oracle for a keyboard: the terminal driver's ISIG/ICANON
processing is the *application's* concern; the keyboard's job is to deliver
the right bytes. Usage: term-probe.py <out-file>
"""
import os
import sys
import termios
import tty

out_path = sys.argv[1] if len(sys.argv) > 1 else "/tmp/term-probe.out"
fd = sys.stdin.fileno()

old = termios.tcgetattr(fd)
tty.setraw(fd)
try:
    with open(out_path, "a", buffering=1) as out:
        out.write("ready\n")
        out.flush()
        while True:
            try:
                data = os.read(fd, 4096)
            except OSError:
                break
            if not data:
                out.write("eof\n")
                out.flush()
                break
            out.write(data.hex(" ") + " ")
            out.flush()
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
