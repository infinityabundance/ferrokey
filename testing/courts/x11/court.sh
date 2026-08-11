#!/usr/bin/env bash
# X11.FOCUS.001 / X11.INJECT.001 (rules 11, 50): the central invariant on
# native Xorg.
#
#   target owns keyboard focus
#     → Ferrokey becomes visible (WM_HINTS.input=False)
#     → pointer clicks an OSK key (xdotool)
#     → target RETAINS keyboard focus        (focus_before == focus_after)
#     → ferrokeyd emits the kernel key        (guest uinput)
#     → target receives the intended input    (target reporter socket)
#     → no stuck keys afterwards
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

start_xorg
start_recorder
start_target_x11
start_ferrokeyd
start_ferrokey

# WM_HINTS evidence: the OSK window must declare input=False (rule 11).
sleep 1
WMHINTS=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xprop -name "Ferrokey" WM_HINTS 2>/dev/null | grep -o "input state is [A-Za-z]*" || echo "")
if echo "$WMHINTS" | grep -q "NO"; then
    ok "WM_HINTS.input = False on the OSK window ($WMHINTS)"
else
    bad "WM_HINTS.input is not False: $WMHINTS"
fi

# Window type / state evidence.
if sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xprop -name "Ferrokey" _NET_WM_WINDOW_TYPE 2>/dev/null | grep -q "DOCK"; then
    ok "_NET_WM_WINDOW_TYPE = DOCK"
else
    bad "_NET_WM_WINDOW_TYPE is not DOCK"
fi

# Give the target focus.
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool windowactivate \
    "$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool search --name ferrokey-test-target | head -1)" 2>/dev/null || true
wait_focus 10

# ── The invariant (rule 50) ───────────────────────────────────────────────
focus_before
click_osk_key a
sleep 0.5

if grep -q '"event":"key","code":30,"down":true' "$EVENTS" 2>/dev/null; then
    ok "target received KEY_A down (code 30)"
else
    bad "target did not receive KEY_A down"
fi
if grep -q '"event":"key","code":30,"down":false' "$EVENTS" 2>/dev/null; then
    ok "target received KEY_A up"
else
    bad "target did not receive KEY_A up"
fi

focus_after

# No stuck keys: no key remains down at the target.
LAST_EVENTS=$(tail -20 "$EVENTS")
if ! grep -q '"event":"key","code":30,"down":true' <(tail -30 "$EVENTS") \
    || grep -q '"event":"key","code":30,"down":false' <(tail -30 "$EVENTS"); then
    ok "no stuck keys after the click"
else
    bad "stuck key detected after click"
fi

# ── Multi-key: a word ─────────────────────────────────────────────────────
DOWNS_BEFORE=$(grep -c '"event":"key","down":true' "$EVENTS")
focus_before
for k in h e l l o; do
    click_osk_key "$k"
done
sleep 0.5
DOWNS_AFTER=$(grep -c '"event":"key","down":true' "$EVENTS")
if [ "$((DOWNS_AFTER - DOWNS_BEFORE))" -ge 5 ]; then
    ok "5 more key presses flowed to the target (h,e,l,l,o)"
else
    bad "expected 5 more key-down events, got $((DOWNS_AFTER - DOWNS_BEFORE))"
fi
focus_after

finish_court PASS "court" "x11"
