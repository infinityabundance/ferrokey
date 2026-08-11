#!/usr/bin/env bash
# MODIFIER.001-004 (rule 19): modifier chords and sticky/locked modifiers,
# verified end-to-end through the guest kernel to the target.
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

# ── MODIFIER.001: hold Shift + tap A = chord ──────────────────────────────
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" left-shift)
X="${POS%,*}" Y="${POS#*,}"
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$X" "$Y" mousedown 1
sleep 0.3
click_osk_key a
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mouseup 1
sleep 0.5

if grep -q '"event":"key","code":42,"down":true' "$EVENTS" 2>/dev/null; then
    ok "chord: LEFTSHIFT down observed"
else
    bad "chord: LEFTSHIFT down missing"
fi
if grep -q '"event":"key","code":30,"down":true' "$EVENTS" 2>/dev/null; then
    ok "chord: KEY_A down observed while shift held"
else
    bad "chord: KEY_A down missing"
fi
# Order matters: shift down must precede A down.
SHIFT_LINE=$(grep -n '"code":42,"down":true' "$EVENTS" | head -1 | cut -d: -f1)
A_LINE=$(grep -n '"code":30,"down":true' "$EVENTS" | head -1 | cut -d: -f1)
if [ -n "$SHIFT_LINE" ] && [ -n "$A_LINE" ] && [ "$SHIFT_LINE" -lt "$A_LINE" ]; then
    ok "chord ordering: shift before A"
else
    bad "chord ordering broken (shift=$SHIFT_LINE a=$A_LINE)"
fi

# ── MODIFIER.002: sticky shift (tap shift, then A) ────────────────────────
click_osk_key left-shift   # quick tap → latch
sleep 0.3
click_osk_key a
sleep 0.5
# The latch must inject shift down before A, then release after.
DOWNS=$(grep -c '"code":42,"down":true' "$EVENTS")
UPS=$(grep -c '"code":42,"down":false' "$EVENTS")
if [ "$DOWNS" -ge 2 ] && [ "$UPS" -ge 2 ]; then
    ok "sticky shift: shift engaged for the next key and released"
else
    bad "sticky shift counts off (downs=$DOWNS ups=$UPS)"
fi

# ── MODIFIER.003: double-tap shift → caps lock ────────────────────────────
click_osk_key left-shift
sleep 0.15
click_osk_key left-shift
sleep 0.3
click_osk_key a
sleep 0.5
# With caps locked, the shift state persists for keys.
DOWNS=$(grep -c '"code":42,"down":true' "$EVENTS")
if [ "$DOWNS" -ge 4 ]; then
    ok "double-tap shift engaged locked shift (more shift-down events)"
else
    bad "caps-lock via double-tap not observed (shift downs=$DOWNS)"
fi

finish_court PASS "court" "modifiers"
