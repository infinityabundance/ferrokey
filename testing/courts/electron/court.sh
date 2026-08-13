#!/usr/bin/env bash
# ELECTRON.001-005 (rule 53): Electron is its own court.
#
# Electron runs a Chromium shell but is NOT the Chromium browser: focus and
# input semantics differ, so support is proven here, separately. The target
# app reports raw key transitions (DOM codes), focus and input text over the
# reporter socket.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

export OSK_VIEW=full

start_xorg
start_recorder
start_ferrokeyd
start_electron "$PAYLOAD/electron"
start_ferrokey "$PAYLOAD/fixtures/ferrokey-full.yaml"

if ! wait_event '"event":"ready"' 120; then
    bad "electron target never became ready"
    tail -30 "$OUT/electron.log"
    finish_court FAIL "phase" "electron-start"
fi
ok "electron target ready"

# Focus the window and click into the input field (below the OSK at y=480).
click_fraction ferrokey-test-target-electron 50 50
wait_focus 30

# ── ELECTRON.001: plain text ───────────────────────────────────────────────
focus_before
click_osk_key h
click_osk_key i
sleep 1
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1)
if echo "$LAST" | grep -q '"text":"hi"'; then
    ok "electron: typed 'hi'"
else
    bad "electron: 'hi' missing; last text: $LAST"
fi
focus_after

# ── ELECTRON.002: Ctrl+A select-all + retype ───────────────────────────────
click_osk_key left-ctrl
click_osk_key a
click_osk_key z
sleep 1
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1)
if echo "$LAST" | grep -q '"text":"z"'; then
    ok "electron: Ctrl+A selected all and typing replaced it"
else
    bad "electron: Ctrl+A failed; last text: $LAST"
fi

# ── ELECTRON.003: arrow navigation ─────────────────────────────────────────
click_osk_key a
click_osk_key b
click_osk_key left
click_osk_key x
sleep 1
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1)
if echo "$LAST" | grep -q '"text":"zaxb"'; then
    ok "electron: Left arrow moved the caret (ab + Left + x → axb)"
else
    bad "electron: arrow navigation failed; last text: $LAST"
fi

# ── ELECTRON.004: genuine key transitions, chord ordering ─────────────────
click_osk_key left-ctrl        # tap → latch
click_osk_key c
sleep 1
CTRL_LINE=$(grep -n '"code":"ControlLeft","down":true' "$EVENTS" | head -1 | cut -d: -f1)
C_LINE=$(grep -n '"code":"KeyC","down":true' "$EVENTS" | head -1 | cut -d: -f1)
if [ -n "$CTRL_LINE" ] && [ -n "$C_LINE" ] && [ "$CTRL_LINE" -lt "$C_LINE" ]; then
    ok "electron: ControlLeft down preceded KeyC down (real chord)"
else
    bad "electron: chord ordering broken (ctrl=$CTRL_LINE c=$C_LINE)"
fi
if grep -q '"code":"KeyC","down":false' "$EVENTS" 2>/dev/null; then
    ok "electron: KeyC released"
else
    bad "electron: KeyC never released (stuck key)"
fi
if grep -q '"code":"ControlLeft","down":false' "$EVENTS" 2>/dev/null; then
    ok "electron: ControlLeft released"
else
    bad "electron: ControlLeft stuck down"
fi

# ── ELECTRON.005: focus preserved throughout ───────────────────────────────
focus_before
click_osk_key e
sleep 0.5
focus_after

finish_court "court" "electron"
