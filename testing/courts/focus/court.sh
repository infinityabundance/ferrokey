#!/usr/bin/env bash
# FOCUS.001 (rules 16, 50): the central focus-preservation invariant against
# each widget toolkit target, with real text verification.
#
#   FOCUS.001.gtk / FOCUS.001.qt / FOCUS.001.slint / FOCUS.001.x11
#
# usage: court.sh <target: gtk|qt|slint|x11>
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

TARGET="${1:-gtk}"
echo "== focus court: target=$TARGET"

start_xorg
start_recorder

case "$TARGET" in
    gtk) start_target_gtk ;;
    qt)  start_target_qt ;;
    slint) start_target_slint ;;
    x11) start_target_x11 ;;
    *)   bad "unknown target $TARGET"; finish_court FAIL "target" "$TARGET" ;;
esac

start_ferrokeyd
start_ferrokey

# Focus the target window.
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool windowactivate \
    "$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool search --name ferrokey-test-target | head -1)" 2>/dev/null || true
wait_focus 10

focus_before
click_osk_key a
sleep 0.5
focus_after

# Text oracle: the widget received the character (rule 17).
case "$TARGET" in
    gtk|qt|slint)
        if grep -q '"event":"text","text":"a"' "$EVENTS" 2>/dev/null \
            || grep -q '"event":"text","text":"a"' "$EVENTS" 2>/dev/null; then
            ok "target text contains the typed character 'a'"
        else
            bad "target text did not receive 'a'"
            tail -5 "$EVENTS"
        fi
        ;;
    x11)
        if grep -q '"event":"key","code":30,"down":true' "$EVENTS" 2>/dev/null; then
            ok "x11 target received KEY_A"
        else
            bad "x11 target did not receive KEY_A"
        fi
        ;;
esac

# Stuck-key check: every down has a matching up at the end.
UNBALANCED=$(python3 - <<'EOF'
import json
down = set()
bad = 0
for line in open("/home/court/court-output/events.log"):
    try:
        ev = json.loads(line)
    except Exception:
        continue
    if ev.get("event") != "key":
        continue
    code = ev["code"]
    if ev["down"]:
        down.add(code)
    else:
        if code in down:
            down.discard(code)
        else:
            bad += 1
print(len(down) + bad)
EOF
)
if [ "$UNBALANCED" = "0" ]; then
    ok "no stuck or unbalanced keys"
else
    bad "unbalanced key events: $UNBALANCED"
fi

finish_court PASS "target" "$TARGET"
