#!/usr/bin/env bash
# REPEAT.001-003 (rule 22): deterministic repeat, verified at the DEVICE
# level (evtest on the Ferrokey uinput node). The target-side count would be
# ambiguous because X11 has its own autorepeat; the device is the oracle.
#
#   pointer down → immediate key-down (value 1)
#   after repeat delay → repeats begin (value 2 — kernel autorepeat semantics;
#   the input core filters repeated value=1 events for held keys, so the
#   repeat engine emits EV_KEY value=2, which passes through untouched)
#   pointer up → repeats stop, key-up emitted (value 0)
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

start_xorg
start_recorder
start_target_x11
start_ferrokeyd
start_ferrokey

NODE=$(ferrokey_device_node)
if [ -z "$NODE" ]; then
    bad "cannot find Ferrokey device node"
    finish_court FAIL "court" "repeat"
fi
echo "Ferrokey device node: /dev/input/$NODE"

DEVLOG="$OUT/device-events.log"
# stdbuf -oL: line-buffer evtest's output so nothing is lost when it is
# killed at the end of the window.
sudo -u root timeout 10 stdbuf -oL evtest --grab "/dev/input/$NODE" > "$DEVLOG" 2>&1 &
EVTEST_PID=$!
sleep 1

# ── REPEAT.001: hold 'a' 1.2s ─────────────────────────────────────────────
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" a)
X="${POS%,*}" Y="${POS#*,}"
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$X" "$Y" mousedown 1
sleep 1.2
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mouseup 1
sleep 0.6

A_PRESSES=$(grep -cE "code 30 \(KEY_A\), value [12]" "$DEVLOG" || true)
A_UPS=$(grep -c "code 30 (KEY_A), value 0" "$DEVLOG" || true)
if [ "$A_PRESSES" -ge 8 ]; then
    ok "repeat: device saw >= 8 KEY_A press events while held (got $A_PRESSES)"
else
    bad "repeat: expected >= 8 KEY_A presses, got $A_PRESSES"
fi
if [ "$A_UPS" -ge 1 ]; then
    ok "repeat: final KEY_A release emitted"
else
    bad "repeat: no final KEY_A release"
fi

# ── REPEAT.002: Backspace repeats ─────────────────────────────────────────
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" backspace)
X="${POS%,*}" Y="${POS#*,}"
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$X" "$Y" mousedown 1
sleep 1.0
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mouseup 1
sleep 0.6
B_PRESSES=$(grep -cE "code 14 \(KEY_BACKSPACE\), value [12]" "$DEVLOG" || true)
if [ "$B_PRESSES" -ge 5 ]; then
    ok "repeat: backspace repeats while held (got $B_PRESSES)"
else
    bad "repeat: backspace did not repeat (got $B_PRESSES)"
fi

# ── REPEAT.003: modifiers must NOT repeat ─────────────────────────────────
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" left-shift)
X="${POS%,*}" Y="${POS#*,}"
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$X" "$Y" mousedown 1
sleep 1.0
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mouseup 1
sleep 0.6
S_PRESSES=$(grep -cE "code 42 \(KEY_LEFTSHIFT\), value [12]" "$DEVLOG" || true)
if [ "$S_PRESSES" -eq 1 ]; then
    ok "repeat: modifier does not auto-repeat (got $S_PRESSES press)"
else
    bad "repeat: modifier repeated ($S_PRESSES presses)"
fi

kill "$EVTEST_PID" 2>/dev/null || true
finish_court "court" "repeat"
