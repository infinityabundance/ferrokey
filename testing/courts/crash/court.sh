#!/usr/bin/env bash
# CRASH.MODIFIER.001 / CRASH.MODIFIER.002 (rules 20, 42): crash recovery.
#
#   * UI holds LEFTCTRL down, then SIGTERM / SIGKILL the UI
#   * the daemon must release every held key (guest-kernel verification via
#     the target's key events)
#   * the daemon must survive UI restarts
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

start_xorg
start_recorder
start_target_x11
start_ferrokeyd
start_ferrokey

sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool windowactivate \
    "$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool search --name ferrokey-test-target | head -1)" 2>/dev/null || true
wait_focus 10

# Hold LEFTCTRL (code 29) via the OSK: press and keep it down.
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" left-ctrl)
X="${POS%,*}" Y="${POS#*,}"
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$X" "$Y" mousedown 1
sleep 0.5

if grep -q '"event":"key","code":29,"down":true' "$EVENTS" 2>/dev/null; then
    ok "target observed LEFTCTRL down (held)"
else
    bad "target did not observe LEFTCTRL down"
fi

# ── CRASH.MODIFIER.001: SIGTERM the UI while it holds the key ─────────────
echo "== SIGTERM ferrokey UI while LEFTCTRL is held"
kill -TERM "$FERROKEY_PID" 2>/dev/null || true
sleep 1
if grep -q '"event":"key","code":29,"down":false' "$EVENTS" 2>/dev/null; then
    ok "SIGTERM: held LEFTCTRL released (target saw key-up)"
else
    bad "SIGTERM: LEFTCTRL NOT released"
fi

# ── CRASH.MODIFIER.002: SIGKILL the UI while it holds a key ───────────────
# Restart the UI (the daemon survived the first crash), hold a key, SIGKILL.
start_ferrokey
sleep 2
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool windowactivate \
    "$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool search --name ferrokey-test-target | head -1)" 2>/dev/null || true
wait_focus 10

POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" left-shift)
X="${POS%,*}" Y="${POS#*,}"
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$X" "$Y" mousedown 1
sleep 0.5

echo "== SIGKILL ferrokey UI while LEFTSHIFT is held"
kill -KILL "$FERROKEY_PID" 2>/dev/null || true
sleep 1
if grep -q '"event":"key","code":42,"down":false' "$EVENTS" 2>/dev/null; then
    ok "SIGKILL: held LEFTSHIFT released (target saw key-up)"
else
    bad "SIGKILL: LEFTSHIFT NOT released"
fi

# Daemon must still accept connections (daemon restart court, rule 27).
if python3 "$PAYLOAD/courts/fk-client.py" --socket /tmp/ferrokeyd.sock handshake release-all; then
    ok "daemon alive and functional after UI crashes"
else
    bad "daemon not functional after UI crashes"
fi

finish_court PASS "court" "crash"
