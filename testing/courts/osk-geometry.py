#!/usr/bin/env python3
"""Compute the on-screen (x, y) center of an OSK key, in physical pixels.

Mirrors the row layout + width table in apps/ferrokey/src/main.rs
(`set_keyboard_rows`) so the courts click exactly the right pixel.

OSK window: 920 x 342 at (0,0). Keyboard padding 6, key spacing 6,
key height 52, base key width 58 (min 44), wide keys 1.6x, space 6x.
"""
import sys

WIDTH = 920
PAD = 6
SPACING = 6
KEY_H = 52
KEY_W = 58

ROWS = [
    ["escape", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12"],
    ["grave", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "minus", "equal", "backspace"],
    ["tab", "q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "left-bracket", "right-bracket", "backslash"],
    ["caps-lock", "a", "s", "d", "f", "g", "h", "j", "k", "l", "semicolon", "apostrophe", "enter"],
    ["left-shift", "z", "x", "c", "v", "b", "n", "m", "comma", "dot", "slash", "right-shift"],
    ["left-ctrl", "left-meta", "left-alt", "space", "right-alt", "menu", "right-ctrl"],
]

WIDE = {"escape", "backspace", "tab", "caps-lock", "enter", "left-shift", "right-shift"}


def key_width(name: str) -> float:
    if name == "space":
        return 6.0
    if name in WIDE:
        return 1.6
    return 1.0


def center(name: str):
    for row_idx, row in enumerate(ROWS):
        if name not in row:
            continue
        x = PAD
        for k in row:
            w = max(44.0, key_width(k) * KEY_W)
            if k == name:
                cx = x + w / 2.0
                cy = PAD + row_idx * (KEY_H + SPACING) + KEY_H / 2.0
                return int(cx), int(cy)
            x += w + SPACING
    return None


if __name__ == "__main__":
    name = sys.argv[1]
    pos = center(name)
    if pos is None:
        print(f"no such key: {name}", file=sys.stderr)
        sys.exit(1)
    print(f"{pos[0]},{pos[1]}")
