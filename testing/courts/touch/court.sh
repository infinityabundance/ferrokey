#!/usr/bin/env bash
# TOUCH.001-003 (Phase 1, rule 26): touch input through the real kernel →
# libinput → Xorg → XI2 → Ferrokey path.
#
# A uinput touchscreen (created by the fake-touch helper) is a REAL input
# device inside the guest: Xorg's libinput driver attaches it via udev, and
# touching the OSK delivers XI2 TouchBegin/TouchEnd to the OSK window. The
# OSK must respond exactly like a click: key down → key up, target keeps
# keyboard focus.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

start_xorg
start_recorder
start_target_x11
start_ferrokeyd

# ── Create the guest touchscreen (root: /dev/uinput) ───────────────────────
# The helper holds the uinput fd open and serves commands from a fifo (a
# uinput device dies with its fd, so one-shot invocations could never work).
sudo rm -f /tmp/fake-touch.cmd
sudo mkfifo /tmp/fake-touch.cmd
sudo chmod 666 /tmp/fake-touch.cmd
sudo "$PAYLOAD/bin/fake-touch" create < /tmp/fake-touch.cmd >"$OUT/fake-touch.log" 2>&1 &
FAKETOUCH_PID=$!
# A held-open writer unblocks the helper's read so the device gets created.
# The keeper must NOT hold the ssh session's stdout/stderr or the court's
# ssh never sees EOF and the evidence collection hangs.
sudo sh -c 'exec 3>/tmp/fake-touch.cmd; sleep 600' >/dev/null 2>&1 &
FIFO_KEEPER=$!
sleep 1
if ! kill -0 "$FAKETOUCH_PID" 2>/dev/null; then
    bad "fake-touch create failed"
    cat "$OUT/fake-touch.log"
    finish_court FAIL "phase" "touchscreen-create"
fi

touch_fifo() { sudo sh -c "echo \"$1\" > /tmp/fake-touch.cmd"; }

# Wait for Xorg/libinput to attach the device (udev hotplug is async).
TOUCH_READY=0
for _ in $(seq 1 50); do
    if sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xinput list 2>/dev/null | grep -q "Ferrokey Court Touchscreen"; then
        TOUCH_READY=1
        break
    fi
    sleep 0.2
done
if [ "$TOUCH_READY" = "1" ]; then
    ok "Xorg attached the court touchscreen"
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xinput list 2>/dev/null | grep "Ferrokey Court Touchscreen" >>"$OUT/touchscreen.txt" || true
else
    bad "touchscreen never appeared in xinput"
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xinput list >"$OUT/xinput-list.txt" 2>&1 || true
    finish_court FAIL "phase" "touchscreen-attach"
fi

start_ferrokey

focus_target
wait_focus 10

# ── TOUCH.001: a touch tap on the 'a' key delivers KEY_A down+up ───────────
focus_before
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" a)
X="${POS%,*}" Y="${POS#*,}"
touch_fifo "tap $X $Y"
sleep 0.8
if grep -q '"event":"key","code":38,"down":true' "$EVENTS" 2>/dev/null; then
    ok "touch tap delivered KEY_A down (X11 keycode 38)"
else
    bad "touch tap did not deliver KEY_A down"
    grep '"event":"key"' "$EVENTS" | tail -3 || true
fi
if grep -q '"event":"key","code":38,"down":false' "$EVENTS" 2>/dev/null; then
    ok "touch release delivered KEY_A up"
else
    bad "touch tap did not deliver KEY_A up"
    grep '"event":"key"' "$EVENTS" | tail -3 || true
fi
focus_after

# ── TOUCH.002: a hold + move + lift behaves like a press ───────────────────
# down on 'h', move over the key, up: the OSK must emit H down then H up.
H_POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" h)
HX="${H_POS%,*}" HY="${H_POS#*,}"
touch_fifo "down $HX $HY"
sleep 0.4
touch_fifo "move $((HX + 4)) $HY"
sleep 0.4
touch_fifo "up"
sleep 0.8
if grep -q '"event":"key","code":43,"down":true' "$EVENTS" 2>/dev/null; then
    ok "touch hold delivered KEY_H down (X11 keycode 43)"
else
    bad "touch hold did not deliver KEY_H down"
    grep '"event":"key"' "$EVENTS" | tail -3 || true
fi
if grep -q '"event":"key","code":43,"down":false' "$EVENTS" 2>/dev/null; then
    ok "touch lift delivered KEY_H up"
else
    bad "touch lift did not deliver KEY_H up"
    grep '"event":"key"' "$EVENTS" | tail -3 || true
fi

# ── TOUCH.003: no stuck keys after the interaction ─────────────────────────
DOWNS=$(grep -c '"down":true' "$EVENTS" || true)
UPS=$(grep -c '"down":false' "$EVENTS" || true)
if [ "$DOWNS" -eq "$UPS" ] && [ "$DOWNS" -gt 0 ]; then
    ok "no stuck keys after touch interactions ($DOWNS down / $UPS up)"
else
    bad "stuck keys after touch (downs=$DOWNS ups=$UPS)"
    tail -30 "$EVENTS"
fi

finish_court "court" "touch"
