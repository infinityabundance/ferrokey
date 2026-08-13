#!/usr/bin/env bash
# TEXTMODE.001-003 (Phase 1): text mode types characters through the layout
# engine, while modifiers and navigation keys stay real kernel keys.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

start_xorg
start_recorder
start_target_gtk
start_ferrokeyd
start_ferrokey "$PAYLOAD/fixtures/ferrokey-text.yaml"

focus_target
wait_focus 10

# ── TEXTMODE.001: a plain word is typed via layout chords ──────────────────
focus_before
for k in h e l l o; do
    click_osk_key "$k"
done
sleep 0.6
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1)
if echo "$LAST" | grep -q '"text":"hello"'; then
    ok "text mode typed 'hello'"
else
    bad "text mode did not type hello; last text event: $LAST"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi
focus_after

# ── TEXTMODE.002: sticky shift produces a capital ──────────────────────────
# A quick tap of the OSK shift latches it; the next character resolves to its
# shifted symbol and the latch is consumed exactly once.
click_osk_key left-shift
sleep 0.2
click_osk_key a
sleep 0.6
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1)
if echo "$LAST" | grep -q '"text":"helloA"'; then
    ok "sticky shift produced 'A' in text mode"
else
    bad "sticky shift failed; last text event: $LAST"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi

# ── TEXTMODE.003: navigation keys still work as real kernel keys ───────────
# Backspace is not a character: it must fall through to the keyboard path and
# delete the 'A' through the kernel.
click_osk_key backspace
sleep 0.6
LAST=$(grep '"event":"text"' "$EVENTS" | tail -1)
if echo "$LAST" | grep -q '"text":"hello"'; then
    ok "backspace worked as a real key in text mode"
else
    bad "backspace did not delete; last text event: $LAST"
    grep '"event":"text"' "$EVENTS" | tail -3 || true
fi

finish_court "court" "text-mode"
