#!/usr/bin/env bash
# ALTGR.001-003 (Phase 1): AltGr as a genuine held modifier in keyboard mode.
#
# Holding the OSK's right-alt (a real KEY_RIGHTALT through the kernel) and
# tapping a letter must produce the AltGr symbol: with the us-intl desktop
# keymap, AltGr+E → é. Also verifies that a quick AltGr tap latches like
# Shift (sticky AltGr), and that focus survives the whole interaction.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

start_xorg
start_recorder
start_target_gtk

# The OSK layout and the desktop keymap must agree: us-intl AltGr+E → é.
# (The authoritative per-device application happens after the virtual
# keyboard hot-plugs, below.)
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" setxkbmap us -variant intl
sleep 0.5

start_ferrokeyd
start_ferrokey "$PAYLOAD/fixtures/ferrokey-intl.yaml"

# us-intl must apply to EVERY keyboard device by its XInput id: the virtual
# keyboard hot-plugs after the daemon starts and would otherwise keep the
# server's default 'us' map, so AltGr+E would not produce é (same lesson as
# the dead-keys and full-desktop courts).
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xinput list > "$OUT/xinput-list.txt" 2>&1 || true
for id in $(awk -F'id=' '/id=[0-9]+/ {print $2}' "$OUT/xinput-list.txt" | awk '{print $1}' | sort -n | uniq || true); do
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" setxkbmap -device "$id" us -variant intl 2>/dev/null || true
done
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" setxkbmap us -variant intl
sleep 0.5

focus_target
wait_focus 10

# ── ALTGR.001: hold right-alt, tap e → é ───────────────────────────────────
# A single pointer cannot press two keys at once, so the chord uses two
# buttons: hold right-alt with button 1 while tapping e with button 2.
focus_before
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" right-alt)
X="${POS%,*}" Y="${POS#*,}"
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$X" "$Y" mousedown 1
sleep 0.3
click_osk_key_button e 2
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mouseup 1
sleep 0.6
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1 || true)
if echo "$LAST" | grep -q '"text":"é"'; then
    ok "held AltGr + e produced é"
else
    bad "held AltGr + e did not produce é; last text event: $LAST"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi
focus_after

# ── ALTGR.002: sticky AltGr (quick tap) latches like shift ─────────────────
# A fast tap of right-alt must latch AltGr; the next letter is AltGr'd, then
# the latch is consumed.
click_osk_key right-alt
sleep 0.2
click_osk_key a
sleep 0.6
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1 || true)
if echo "$LAST" | grep -q '"text":"éá"'; then
    ok "sticky AltGr + a produced á (us-intl AltGr+a)"
else
    bad "sticky AltGr failed; last text event: $LAST"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi

# ── ALTGR.003: after the latch is consumed, the next key is unmodified ─────
click_osk_key b
sleep 0.6
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1 || true)
if echo "$LAST" | grep -q '"text":"éáb"'; then
    ok "AltGr latch consumed: next key typed plain 'b'"
else
    bad "AltGr latch was not consumed; last text event: $LAST"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi

finish_court "court" "altgr"
