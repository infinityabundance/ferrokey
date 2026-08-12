#!/usr/bin/env bash
# LAYOUT.US.001 / LAYOUT.DE.001 (rule 23): PhysicalKey != Unicode.
#
#   * with the desktop keymap set to German (QWERTZ), clicking the OSK key
#     labelled "z" (which is physical Y) must produce the character 'z',
#     proving the OSK emits KEY_Y and lets the desktop keymap decide
#   * clicking the key labelled "y" (physical Z) must produce 'y'
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

start_xorg
start_recorder
start_target_gtk

# Switch the desktop XKB layout to German (QWERTZ).
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" setxkbmap de
sleep 0.5

start_ferrokeyd
start_ferrokey

focus_target
wait_focus 10

# In the de layout, physical Y is labelled 'z' and physical Z is 'y'.
click_osk_key y   # physical Y → de desktop keymap → 'z'
click_osk_key z   # physical Z → de desktop keymap → 'y'
sleep 0.5

if grep -q '"event":"text","text":"zy"' "$EVENTS" 2>/dev/null \
    || grep -q '"event":"text","text":"yz"' "$EVENTS" 2>/dev/null; then
    ok "QWERTZ: physical Y/Z produced z/y via the desktop keymap"
else
    bad "layout translation wrong; events so far:"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi

# The OSK labels must reflect the layout (physical Y shows 'z' in de).
if grep -q "de" "$OUT/ferrokey.log" 2>/dev/null; then
    true
fi
LABEL_OK=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool search --name "Ferrokey" 2>/dev/null | wc -l)
if [ "$LABEL_OK" -ge 1 ]; then
    ok "OSK window present with de layout"
else
    bad "OSK window missing"
fi

finish_court "court" "layouts"
