#!/usr/bin/env bash
# SDL.001-004 (rule 54): genuine key state transitions in an SDL application.
#
# Games and emulators read key-down/key-up events, not text. The SDL target
# reports the raw SDL scancode of every transition (A=4, C=6, F1=58, F5=62,
# LeftCtrl=224), so the court proves the OSK delivers real state changes in
# the right ORDER — something no text-based oracle can show.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

export OSK_VIEW=full

start_xorg
start_recorder
start_sdl
start_ferrokeyd
start_ferrokey "$PAYLOAD/fixtures/ferrokey-full.yaml"

# Park the SDL window below the OSK and give it keyboard focus. No --sync:
# activation can block forever if the WM delays the acknowledge.
position_target_below_osk ferrokey-test-target-sdl 420 120
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" timeout 10 xdotool windowactivate \
    "$(window_of ferrokey-test-target-sdl)"
sleep 0.5
wait_focus 10

# ── SDL.001: a plain key delivers down + up ────────────────────────────────
focus_before
click_osk_key a
sleep 0.5
if grep -q '"event":"key","code":4,"down":true' "$EVENTS" 2>/dev/null; then
    ok "SDL: scancode 4 (A) down"
else
    bad "SDL: A down missing"
    grep '"event":"key"' "$EVENTS" | tail -2 || true
fi
if grep -q '"event":"key","code":4,"down":false' "$EVENTS" 2>/dev/null; then
    ok "SDL: scancode 4 (A) up"
else
    bad "SDL: A up missing"
fi
focus_after

# ── SDL.002: a genuine chord — Ctrl+C with correct ordering ────────────────
# Hold Ctrl with button 1 while tapping C with button 2 (single pointer).
POS=$(osk_key_pos left-ctrl)
X="${POS%,*}" Y="${POS#*,}"
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$X" "$Y" mousedown 1
sleep 0.3
click_osk_key_button c 2
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mouseup 1
sleep 0.5
CTRL_LINE=$(grep -n '"code":224,"down":true' "$EVENTS" | head -1 | cut -d: -f1)
C_LINE=$(grep -n '"code":6,"down":true' "$EVENTS" | head -1 | cut -d: -f1)
if [ -n "$CTRL_LINE" ] && [ -n "$C_LINE" ] && [ "$CTRL_LINE" -lt "$C_LINE" ]; then
    ok "SDL: LeftCtrl(224) down preceded C(6) down"
else
    bad "SDL: chord ordering broken (ctrl=$CTRL_LINE c=$C_LINE)"
fi
if grep -q '"code":224,"down":false' "$EVENTS" 2>/dev/null \
    && grep -q '"code":6,"down":false' "$EVENTS" 2>/dev/null; then
    ok "SDL: Ctrl and C both released"
else
    bad "SDL: stuck key after the chord"
fi

# ── SDL.003: function keys ─────────────────────────────────────────────────
click_osk_key f5
sleep 0.5
if grep -q '"event":"key","code":62,"down":true' "$EVENTS" 2>/dev/null; then
    ok "SDL: F5 (scancode 62) down"
else
    bad "SDL: F5 missing"
fi

# ── SDL.004: navigation keys ───────────────────────────────────────────────
click_osk_key up
click_osk_key left
sleep 0.5
if grep -q '"event":"key","code":82,"down":true' "$EVENTS" 2>/dev/null; then
    ok "SDL: Up (scancode 82) down"
fi
if grep -q '"event":"key","code":80,"down":true' "$EVENTS" 2>/dev/null; then
    ok "SDL: Left (scancode 80) down"
fi

# ── No stuck keys ──────────────────────────────────────────────────────────
DOWNS=$(grep -c '"down":true' "$EVENTS" || true)
UPS=$(grep -c '"down":false' "$EVENTS" || true)
if [ "$DOWNS" -eq "$UPS" ] && [ "$DOWNS" -gt 0 ]; then
    ok "SDL: no stuck keys ($DOWNS down / $UPS up)"
else
    bad "SDL: stuck keys (downs=$DOWNS ups=$UPS)"
fi

finish_court "court" "sdl"
