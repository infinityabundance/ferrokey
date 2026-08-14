#!/usr/bin/env bash
# XWAYLAND.001 (rule 15): Ferrokey's X11 no-focus surface running against
# XWayland, with a native Wayland target — independent of native Xorg.
#
# Contract under test (the X11 no-focus surface over XWayland):
#   * the surface carries the ICCCM no-input contract over XWayland
#     (WM_HINTS.input=False) plus the dock contract
#     (_NET_WM_WINDOW_TYPE=DOCK, _NET_WM_STATE=ABOVE|SKIP_TASKBAR|SKIP_PAGER,
#     override_redirect) — the same properties the native Xorg courts assert;
#   * the surface is interactive: clicking a key emits it through ferrokeyd
#     into the kernel input stack. The emission is observed on the Ferrokey
#     uinput device itself (evtest) — a real kernel path, independent of the
#     compositor's focus routing.
#
# Focus note: sway 1.7 focuses override-redirect XWayland surfaces on map AND
# on click (sway/desktop/xwayland.c unmanaged_handle_map +
# sway/input/seatop_default.c handle_button; the DOCK type is absent from
# wlr_xwayland_or_surface_wants_focus's exemption list, so override-redirect
# windows get keyboard focus by design). The no-focus-preservation contract
# is therefore asserted on the compositors that honor input=False: native
# Xorg (x11 court) and native layer-shell (wayland court).
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

# sway is the window manager here (same rationale as the wayland court).
export COURT_NO_WM=1

sudo mkdir -p /run/user/1000
sudo chown court:court /run/user/1000
sudo chmod 700 /run/user/1000

start_xorg

# ── The court compositor config (shared with the wayland court) ───────────
COMPOSITOR_CONFIG="$OUT/sway-court.config"
cat > "$COMPOSITOR_CONFIG" <<'EOF'
# Ferrokey court compositor (sway / wlroots, nested in the dummy X server).
xwayland enable
seat "*" {
    xcursor_theme default
}
output "*" {
    bg #202020 solid_color
}
EOF

# ── Start sway (wlroots) nested in the dummy X server ─────────────────────
sudo -u "$COURT_USER" env DISPLAY=:0 WLR_BACKENDS=x11 WLR_RENDERER=pixman \
    XDG_RUNTIME_DIR=/run/user/1000 \
    timeout 180 sway -c "$COMPOSITOR_CONFIG" > "$OUT/sway.log" 2>&1 &
sleep 8
SWAY_SOCK=$(ls /run/user/1000/wayland-* 2>/dev/null | head -1 || true)
if [ -n "$SWAY_SOCK" ]; then
    ok "sway running"
else
    bad "sway failed to start"
    tail -20 "$OUT/sway.log"
    finish_court FAIL "court" "xwayland"
fi

# XWayland display: wlroots uses :1 by default when :0 is taken.
export DISPLAY_XW=:1
export WAYLAND_DISPLAY="$(basename "$SWAY_SOCK")"
export XDG_RUNTIME_DIR=/run/user/1000

# ── Compositor output geometry (the wlroots x11-backend window) ───────────
# A failed query MUST fail the court (the old 1280x720 full-screen fallback
# silently mis-clicked the OSK).
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xwininfo -root -tree > "$OUT/xwininfo.txt" 2>/dev/null || true
WINX=0; WINY=0; WINW=0; WINH=0
GEO=$(compositor_geometry) || true
if [ -n "$GEO" ]; then
    set -- $GEO
    WINX=$1; WINY=$2; WINW=$3; WINH=$4
fi
if [ "$WINW" -gt 0 ] && [ "$WINH" -gt 0 ]; then
    ok "compositor output window at ${WINX},${WINY} ${WINW}x${WINH}"
else
    bad "could not determine the wlroots compositor window geometry"
    finish_court FAIL "phase" "compositor-geometry"
fi

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

# The OSK window must exist before its geometry (and properties) can be read.
OSK_WIN=""
for _ in $(seq 1 50); do
    OSK_WIN=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY_XW" xdotool search --name "Ferrokey Virtual Keyboard" 2>/dev/null | head -1 || true)
    [ -n "$OSK_WIN" ] && break
    sleep 0.2
done
if [ -n "$OSK_WIN" ]; then
    ok "XWayland OSK window present"
else
    bad "XWayland OSK window never appeared"
    cat "$OUT/ferrokey.log"
    finish_court FAIL "court" "xwayland"
fi

# ── The X11 no-focus surface contract, as seen over XWayland ──────────────
# WM_HINTS.input=False: the ICCCM No-Input focus model (input = False, no
# WM_TAKE_FOCUS) — the mechanism by which the OSK declines keyboard focus.
# xprop dumps WM_HINTS as "Client accepts input or input focus: False".
WMHINTS=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY_XW" xprop -id "$OSK_WIN" WM_HINTS 2>/dev/null | grep -o "input focus: [A-Za-z]*" || echo "")
if echo "$WMHINTS" | grep -q "False"; then
    ok "WM_HINTS.input = False over XWayland"
else
    bad "WM_HINTS.input over XWayland: $WMHINTS"
fi
# _NET_WM_WINDOW_TYPE=DOCK: the OSK is a dock-type window (never a normal
# app window; the native Xorg court asserts the same contract).
WINTYPE=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY_XW" xprop -id "$OSK_WIN" _NET_WM_WINDOW_TYPE 2>/dev/null | grep -o "_NET_WM_WINDOW_TYPE_DOCK" || echo "")
if [ -n "$WINTYPE" ]; then
    ok "_NET_WM_WINDOW_TYPE = DOCK over XWayland"
else
    bad "_NET_WM_WINDOW_TYPE over XWayland: $(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY_XW" xprop -id "$OSK_WIN" _NET_WM_WINDOW_TYPE 2>/dev/null || true)"
fi
# _NET_WM_STATE=ABOVE|SKIP_TASKBAR|SKIP_PAGER: the dock placement contract.
WMSTATE=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY_XW" xprop -id "$OSK_WIN" _NET_WM_STATE 2>/dev/null || true)
if echo "$WMSTATE" | grep -q "_NET_WM_STATE_ABOVE" \
    && echo "$WMSTATE" | grep -q "_NET_WM_STATE_SKIP_TASKBAR" \
    && echo "$WMSTATE" | grep -q "_NET_WM_STATE_SKIP_PAGER"; then
    ok "_NET_WM_STATE = ABOVE, SKIP_TASKBAR, SKIP_PAGER over XWayland"
else
    bad "_NET_WM_STATE over XWayland: $(echo "$WMSTATE" | tr '\n' ' ')"
fi
# The OSK's real position on the XWayland display (override-redirect windows
# sit where the client requested them — here the top-left corner).
OSK_GEO=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY_XW" xdotool getwindowgeometry --shell "$OSK_WIN" 2>/dev/null || true)
OSK_X=0; OSK_Y=0
if [ -n "$OSK_GEO" ]; then
    eval "$OSK_GEO"
    OSK_X=$X; OSK_Y=$Y
fi
ok "XWayland OSK window at ${OSK_X},${OSK_Y}"

# ── Focus the native wayland target ───────────────────────────────────────
# The OSK occupies the top-left corner, so click the lower part of the
# output, which is the target's surface.
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove $((WINX + WINW / 2)) $((WINY + WINH - 100)) click 1
wait_focus 10

# ── Activate the OSK 'a' key ──────────────────────────────────────────────
# The click travels X11 → wlroots → sway → the XWayland OSK surface → the
# OSK's X11 window on :1, where ferrokey hit-tests 'a' and emits KEY_A
# through ferrokeyd into the kernel input stack. The emission is captured on
# the Ferrokey uinput device itself (evtest grab) — compositor-independent.
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" a)
KX="${POS%,*}" KY="${POS#*,}"
TX=$((WINX + OSK_X + KX))
TY=$((WINY + OSK_Y + KY))
EV_NODE=$(ferrokey_device_node)
if [ -z "$EV_NODE" ]; then
    bad "ferrokey uinput device node not found"
    finish_court FAIL "phase" "device-node"
fi
( sleep 1.2; sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$TX" "$TY" click 1 ) &
timeout 4 sudo -u root evtest --grab "/dev/input/$EV_NODE" > "$OUT/evtest.log" 2>&1 || true
sleep 1
if grep -q "KEY_A" "$OUT/evtest.log" 2>/dev/null; then
    ok "XWayland OSK emitted KEY_A (observed on the Ferrokey uinput device)"
else
    bad "XWayland OSK did not emit KEY_A"
    echo "== evtest.log:"; tail -10 "$OUT/evtest.log" 2>/dev/null
fi

finish_court "court" "xwayland"
