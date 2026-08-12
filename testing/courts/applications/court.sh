#!/usr/bin/env bash
# APPLICATIONS.001 (rule 16): the real-application target matrix.
# GTK, Qt, Slint and the raw X11 target all receive injected input while
# keeping focus. Each target is exercised in its own X session iteration.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

start_xorg
start_ferrokeyd

for TARGET in gtk qt slint x11; do
    echo "== application target: $TARGET"
    start_recorder
    case "$TARGET" in
        gtk) start_target_gtk ;;
        qt)  start_target_qt ;;
        slint) start_target_slint ;;
        x11) start_target_x11 ;;
    esac
    start_ferrokey

    focus_target
    wait_focus 10

    focus_before
    click_osk_key h
    click_osk_key i
    sleep 0.5
    focus_after

    case "$TARGET" in
        gtk|qt|slint)
            if grep -q '"event":"text","text":"hi"' "$EVENTS" 2>/dev/null; then
                ok "$TARGET: typed 'hi'"
            else
                bad "$TARGET: did not receive 'hi'"
                tail -3 "$EVENTS"
            fi
            ;;
        x11)
            if grep -q '"code":43,"down":true' "$EVENTS" 2>/dev/null \
                && grep -q '"code":31,"down":true' "$EVENTS" 2>/dev/null; then
                ok "x11: received KEY_H + KEY_I"
            else
                bad "x11: missing key events"
            fi
            ;;
    esac

    # Teardown between targets.
    kill "$FERROKEY_PID" 2>/dev/null || true
    kill "$TARGET_PID" 2>/dev/null || true
    pkill -f ferrokey-test-target 2>/dev/null || true
    sleep 1
    : > "$EVENTS"
done

finish_court "court" "applications"
