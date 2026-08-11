#!/usr/bin/env bash
# XWAYLAND.001 (rule 15): Ferrokey's X11 no-focus surface running against
# XWayland, with a native Wayland target — independent of native Xorg.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

mkdir -p /run/user/1000
chown court:court /run/user/1000
chmod 700 /run/user/1000

start_xorg

sudo -u "$COURT_USER" env DISPLAY=:0 WLR_BACKENDS=x11 LIBGL_ALWAYS_SOFTWARE=1 \
    XDG_RUNTIME_DIR=/run/user/1000 dbus-run-session -- \
    wayfire --socket wayland-court-0 > "$OUT/wayfire.log" 2>&1 &
sleep 5
if ls /run/user/1000/wayland-court-0 >/dev/null 2>&1; then
    ok "wayfire running"
else
    bad "wayfire failed to start"
    tail -20 "$OUT/wayfire.log"
    finish_court FAIL "court" "xwayland"
fi

# XWayland display: wlroots uses :1 by default when :0 is taken.
export DISPLAY_XW=:1
export WAYLAND_DISPLAY=wayland-court-0
export XDG_RUNTIME_DIR=/run/user/1000

start_recorder
start_target_wayland
start_ferrokeyd

# Wait for XWayland to be up.
sleep 3
if sudo -u "$COURT_USER" env DISPLAY="$DISPLAY_XW" xdpyinfo >/dev/null 2>&1; then
    ok "XWayland available on $DISPLAY_XW"
else
    bad "XWayland not available"
    finish_court FAIL "court" "xwayland"
fi

# ── Ferrokey with the X11 backend against XWayland ────────────────────────
# Force the X11 path: no WAYLAND_DISPLAY, DISPLAY=:1 (XWayland).
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY_XW" WAYLAND_DISPLAY= XDG_RUNTIME_DIR=/run/user/1000 \
    "$PAYLOAD/bin/ferrokey" --config "$PAYLOAD/fixtures/ferrokey.yaml" \
    > "$OUT/ferrokey.log" 2>&1 &
FERROKEY_PID=$!
sleep 4
if kill -0 "$FERROKEY_PID" 2>/dev/null; then
    ok "ferrokey UI running on XWayland"
else
    bad "ferrokey UI exited on XWayland"
    cat "$OUT/ferrokey.log"
    finish_court FAIL "court" "xwayland"
fi

# WM_HINTS.input=False over XWayland.
WMHINTS=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY_XW" xprop -name "Ferrokey" WM_HINTS 2>/dev/null | grep -o "input state is [A-Za-z]*" || echo "")
if echo "$WMHINTS" | grep -q "NO"; then
    ok "WM_HINTS.input = False over XWayland"
else
    bad "WM_HINTS.input over XWayland: $WMHINTS"
fi

# ── Focus the native wayland target ───────────────────────────────────────
sudo -u "$COURT_USER" env DISPLAY=:0 xdotool mousemove 300 150 click 1
wait_focus 10

focus_before
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" a)
X="${POS%,*}" Y="${POS#*,}"
# The OSK is an X11 window on XWayland at (0,378) within the 1280x720
# compositor space.
sudo -u "$COURT_USER" env DISPLAY=:0 xdotool mousemove "$X" "$((Y + 378))" click 1
sleep 0.6

if grep -q '"event":"key","code":30,"down":true' "$EVENTS" 2>/dev/null; then
    ok "wayland target received KEY_A via XWayland-backed OSK"
else
    bad "wayland target did not receive KEY_A"
fi
focus_after

finish_court PASS "court" "xwayland"
