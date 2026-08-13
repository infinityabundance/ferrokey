#!/usr/bin/env bash
# DEADKEYS.001-004 (Phase 1, rule 28): the Ferrokey compose engine end-to-end.
#
# The OSK runs the us-intl layout in TEXT MODE: the apostrophe key is
# Dead(Acute) and the compose key is real. The engine composes:
#
#   ' + e → é        (dead acute, injected as AltGr+E)
#   compose o c → ©  (multi-key compose, injected as AltGr+C)
#
# The desktop keymap is us-intl so the injected AltGr chords land as the
# composed characters in the GTK target, and the target keeps keyboard focus
# the whole time.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

start_xorg
start_recorder
start_target_gtk

# The engine injects physical chords (AltGr+E); the desktop keymap decides the
# resulting character, so it must match the OSK layout. The keymap state is
# captured as evidence: a failure must be attributable (was Level3 active? did
# the chord reach the target as ISO_Level3_Shift?)
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" setxkbmap us -variant intl
sleep 0.5
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" setxkbmap -query > "$OUT/keymap-query.txt" 2>&1 || true
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xkbcomp -xkb :0 "$OUT/keymap.xkb" 2>/dev/null || true
# The injected AltGr key (X11 keycode 108 = evdev RightAlt) must be
# ISO_Level3_Shift under us-intl — prove the keymap, not just hope it.
if grep -q "ISO_Level3_Shift" "$OUT/keymap.xkb" 2>/dev/null; then
    ok "desktop keymap has ISO_Level3_Shift (us-intl active)"
else
    bad "desktop keymap lacks ISO_Level3_Shift — AltGr chords cannot compose"
fi

start_ferrokeyd

# The Ferrokey virtual keyboard is hot-plugged by the daemon; modern Xorg
# translates its events with the MASTER keymap, but prove the device list
# after the plug and re-apply the layout to every keyboard device so the
# AltGr chord translates as ISO_Level3_Shift regardless of per-device maps.
sleep 2
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xinput list > "$OUT/xinput-list.txt" 2>&1 || true
# Apply the layout to EVERY keyboard device by its XInput id (setxkbmap
# -device takes the numeric id, not the name) — hot-plugged virtual
# keyboards start with the server's default map otherwise.
for id in $(awk -F'id=' '/id=[0-9]+/ {print $2}' "$OUT/xinput-list.txt" | awk '{print $1}' | sort -n | uniq || true); do
    sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" setxkbmap -device "$id" us -variant intl 2>/dev/null || true
done
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" setxkbmap -query > "$OUT/keymap-query.txt" 2>&1 || true
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xkbcomp -xkb :0 "$OUT/keymap.xkb" 2>/dev/null || true
sleep 0.5
start_ferrokey "$PAYLOAD/fixtures/ferrokey-intl-text.yaml"

focus_target
wait_focus 10

# ── DEADKEYS.001: dead acute ' + e → é ─────────────────────────────────────
# The OSK window's geometry is captured before the first clicks: a displaced
# first click (reported by the bridge in the UI log) must be attributable.
{
    echo "=== before first clicks ==="
    for w in $(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool search --name 'Ferrokey' 2>/dev/null); do
        echo "$w: $(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowgeometry --shell "$w" 2>/dev/null | tr '\n' ' ')"
    done
} > "$OUT/window-geometry.txt" 2>&1
focus_before
click_osk_key apostrophe
click_osk_key e
{
    echo "=== after first sequence ==="
    for w in $(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool search --name 'Ferrokey' 2>/dev/null); do
        echo "$w: $(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowgeometry --shell "$w" 2>/dev/null | tr '\n' ' ')"
    done
} >> "$OUT/window-geometry.txt" 2>&1
sleep 0.6
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1 || true)
if echo "$LAST" | grep -q '"text":"é"'; then
    ok "' (dead acute) + e composed to é"
else
    bad "' + e did not compose; last text event: $LAST"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi
focus_after

# ── DEADKEYS.002: compose key o c → © ──────────────────────────────────────
click_osk_key compose
click_osk_key o
click_osk_key c
sleep 0.6
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1 || true)
if echo "$LAST" | grep -q '"text":"é©"'; then
    ok "compose o c produced ©"
else
    bad "compose o c failed; last text event: $LAST"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi

# ── DEADKEYS.003: dead acute + a → á ───────────────────────────────────────
click_osk_key apostrophe
click_osk_key a
sleep 0.6
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1)
if echo "$LAST" | grep -q '"text":"é©á"'; then
    ok "dead acute + a composed to á"
else
    bad "dead acute + a failed; last text event: $LAST"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi

# ── DEADKEYS.004: dead key then a non-composing char drops the accent ──────
# ' + q has no composition in the table: X11 semantics drop the accent and
# deliver the base character. (Documented behaviour, pinned here.)
click_osk_key apostrophe
click_osk_key q
sleep 0.6
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1 || true)
if echo "$LAST" | grep -q '"text":"é©áq"'; then
    ok "dead key + non-composing char delivered the base char (accent dropped)"
else
    bad "dead key + q misbehaved; last text event: $LAST"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi

# ── Focus invariant across the whole court ─────────────────────────────────
focus_before
click_osk_key apostrophe
click_osk_key e
sleep 0.6
focus_after

finish_court "court" "dead-keys"
