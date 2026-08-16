#!/usr/bin/env python3
"""Compute the on-screen (x, y) center of an OSK key, in physical pixels.

Mirrors the keyboard view data in apps/ferrokey/src/views.rs (row layout,
width factors, base width, window size) so the courts click exactly the
pixel that the Slint layout renders. A change to views.rs without a matching
change here breaks every court silently — the pinned positions are locked by
`views::tests::pinned_*_geometry`.

Usage:
  osk-geometry.py [--view compact|full] KEY
"""
import sys

PAD = 6
SPACING = 6
KEY_H = 52
MIN_W = 24.0
# The OSK window is the fixed 22px title strip PLUS the keyboard; the keys
# start below it. Must match views::TITLE_H.
TITLE_H = 22

VIEWS = {
    "compact": {
        "width": 936,
        "base": 58.0,
        "rows": [
            ["escape", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "logo"],
            ["grave", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "minus", "equal", "backspace"],
            ["tab", "q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "left-bracket", "right-bracket", "backslash"],
            ["caps-lock", "a", "s", "d", "f", "g", "h", "j", "k", "l", "semicolon", "apostrophe", "enter"],
            ["left-shift", "z", "x", "c", "v", "b", "n", "m", "comma", "dot", "slash", "up"],
            ["left-ctrl", "left-meta", "left-alt", "space", "right-alt", "compose", "menu", "left", "down", "right"],
        ],
        "widths": {},
    },
    "full": {
        "width": 1160,
        "base": 40.0,
        "rows": [
            ["mute", "volume-down", "volume-up", "play-pause", "previous-song", "next-song", "brightness-down", "brightness-up"],
            ["escape", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "f11", "f12", "sysrq", "scroll-lock", "pause"],
            ["grave", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "minus", "equal", "backspace", "insert", "home", "page-up", "num-lock", "kp-divide", "kp-multiply", "kp-subtract"],
            ["tab", "q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "left-bracket", "right-bracket", "backslash", "delete", "end", "page-down", "kp7", "kp8", "kp9", "kp-add"],
            ["caps-lock", "a", "s", "d", "f", "g", "h", "j", "k", "l", "semicolon", "apostrophe", "enter", "up", "kp4", "kp5", "kp6", "kp-enter"],
            ["left-shift", "z", "x", "c", "v", "b", "n", "m", "comma", "dot", "slash", "right-shift", "left", "down", "right", "kp1", "kp2", "kp3", "kp-decimal"],
            ["left-ctrl", "left-meta", "left-alt", "space", "right-alt", "compose", "menu", "right-ctrl", "kp0"],
        ],
        "widths": {
            # overrides for the full view (everything else is 1.0)
            "backspace": 1.7,
            "tab": 1.4,
            "caps-lock": 1.6,
            "enter": 1.6,
            "left-shift": 2.2,
            "right-shift": 2.2,
            "space": 6.0,
            "kp0": 1.6,
        },
    },
    "terminal": {
        "width": 936,
        "base": 58.0,
        "rows": [
            # shortcut row (chord keys carry display names)
            ["Ctrl+C", "Ctrl+D", "Ctrl+Z", "Ctrl+L", "Ctrl+A", "escape", "home", "end"],
            ["grave", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "minus", "equal", "backspace"],
            ["tab", "q", "w", "e", "r", "t", "y", "u", "i", "o", "p", "left-bracket", "right-bracket", "backslash"],
            ["caps-lock", "a", "s", "d", "f", "g", "h", "j", "k", "l", "semicolon", "apostrophe", "enter"],
            ["left-shift", "z", "x", "c", "v", "b", "n", "m", "comma", "dot", "slash", "right-shift"],
            ["left-ctrl", "left-alt", "left-meta", "space", "delete", "left", "down", "up", "right"],
        ],
        "widths": {
            "Ctrl+C": 1.3,
            "Ctrl+D": 1.3,
            "Ctrl+Z": 1.3,
            "Ctrl+L": 1.3,
            "Ctrl+A": 1.3,
            "escape": 1.3,
            "backspace": 1.6,
            "tab": 1.4,
            "caps-lock": 1.4,
            "enter": 1.6,
            "left-shift": 1.8,
            "right-shift": 1.8,
            "left-ctrl": 1.2,
            "left-alt": 1.2,
            "left-meta": 1.2,
            "space": 6.0,
        },
    },
}

# The compact view's wide keys (shared with the full view's defaults where
# not overridden above). The logo button is 0.9 of the base width; `up` is
# 1.0 like the rest of the arrow cluster.
COMPACT_WIDE = {"escape", "backspace", "tab", "caps-lock", "enter", "left-shift", "right-shift"}
COMPACT_LOGO_WIDTH = 0.9

# Physical keys shown with a label override in the UI (position is unaffected,
# but kept here so the mirror documents the full view data).
FULL_LABELS = {"sysrq": "print"}


def key_width(view, name: str, factor: float) -> float:
    return max(MIN_W, factor * view["base"])


def factor(view, name: str) -> float:
    # Per-view width overrides (the full AND terminal views carry their own
    # widths dicts; the terminal view's chords/modifiers/nav widths MUST be
    # applied here or every court click on those keys lands in a gap and the
    # app's hit test returns None — the exact failure the geometry mirror
    # exists to prevent).
    if view["id"] in ("full", "terminal"):
        return view["widths"].get(name, 1.0)
    if name == "logo":
        return COMPACT_LOGO_WIDTH
    if name == "space":
        # Compact: 4.9 (not 6.0) so the arrow cluster aligns under up.
        return 4.9
    if name in COMPACT_WIDE:
        return 1.6
    return 1.0


def center(view, name: str):
    for row_idx, row in enumerate(view["rows"]):
        if name not in row:
            continue
        x = PAD
        for k in row:
            w = key_width(view, k, factor(view, k))
            if k == name:
                cx = x + w / 2.0
                cy = TITLE_H + PAD + row_idx * (KEY_H + SPACING) + KEY_H / 2.0
                return int(cx), int(cy)
            x += w + SPACING
    return None


def main():
    args = sys.argv[1:]
    view_id = "compact"
    if args and args[0] == "--view":
        if len(args) < 2:
            print("--view requires an id", file=sys.stderr)
            sys.exit(2)
        view_id = args[1]
        args = args[2:]
    if not args:
        print(f"usage: osk-geometry.py [--view {'|'.join(VIEWS)}] KEY", file=sys.stderr)
        sys.exit(2)
    if view_id not in VIEWS:
        print(f"unknown view {view_id}", file=sys.stderr)
        sys.exit(2)
    view = dict(VIEWS[view_id])
    view["id"] = view_id
    pos = center(view, args[0])
    if pos is None:
        print(f"no such key: {args[0]}", file=sys.stderr)
        sys.exit(1)
    print(f"{pos[0]},{pos[1]}")


if __name__ == "__main__":
    main()
