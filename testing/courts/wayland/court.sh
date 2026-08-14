#!/usr/bin/env bash
# WAYLAND.LAYER.001 (rules 12, 13, 50): layer-shell focus preservation.
#
# A genuine compositor (sway / wlroots, the reference layer-shell
# implementation) runs nested in the dummy X server inside the VM,
# compositing in pure software (WLR_RENDERER=pixman — no GPU needed). The OSK
# is a layer surface with keyboard_interactivity = none. The invariant:
#
#   interactive_pointer_surface      (OSK receives pointer clicks)
#   AND no_keyboard_focus_acquisition (target keeps focus)
#   AND successful_kernel_input_injection (target receives the key)
#
# Input path: the OSK is activated with REAL pointer clicks. wlroots' X11
# backend implements only pointer + keyboard devices (no touch), so XI2 touch
# from a uinput touchscreen dies at the X server and never becomes wl_touch.
# The layer-shell contract does not require touch: layer surfaces with
# keyboard_interactivity=none receive pointer input while never receiving
# keyboard focus, which is exactly the invariant above (the uinput touchscreen
# → X → app path is exercised by the X11 `touch` court instead).
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

# sway is the window manager here: an X11 WM (openbox) would only reparent+
# decorate the wlroots x11-backend window and offset the compositor's input
# coordinate space from the geometry the court queries. With no WM the window
# is unmanaged at (0,0) — parent-relative == absolute == the pointer space.
export COURT_NO_WM=1

# Make sure the court user has a runtime dir (no login session in the VM).
sudo mkdir -p /run/user/1000
sudo chown court:court /run/user/1000
sudo chmod 700 /run/user/1000

start_xorg

# ── The court compositor config (also used by the xwayland court) ─────────
# wlroots' X11 backend runs sway nested in the dummy X server; the layer
# surface (wayland court) needs nothing special, but the XWayland OSK window
# (xwayland court) must float at the dock position the court expects.
COMPOSITOR_CONFIG="$OUT/sway-court.config"
cat > "$COMPOSITOR_CONFIG" <<'EOF'
# Ferrokey court compositor (sway / wlroots, nested in the dummy X server).
xwayland enable
for_window [title="Ferrokey Virtual Keyboard"] floating enable
for_window [title="Ferrokey Virtual Keyboard"] move position 0 378
seat "*" {
    xcursor_theme default
}
output "*" {
    bg #202020 solid_color
}
EOF

# ── Start sway (wlroots, the reference layer-shell compositor) ─────────────
# WLR_BACKENDS=x11 + WLR_RENDERER=pixman: pure software, no GPU. The output
# is pinned to the dummy screen size (WLR_X11_OUTPUTS) so the touchscreen's
# 1:1 ABS mapping and the court geometry match. wlroots' X11 backend honours
# keyboard_interactivity=none and delivers XI2 touch, which is exactly what
# the layer-shell no-focus + touch contracts need (KWin's X11 backend
# supports neither). sway names its socket wayland-N (it ignores
# WAYLAND_DISPLAY), so the court discovers it instead of assuming a name.
sudo -u "$COURT_USER" env DISPLAY=:0 WLR_BACKENDS=x11 WLR_RENDERER=pixman \
    XDG_RUNTIME_DIR=/run/user/1000 \
    timeout 180 sway -c "$COMPOSITOR_CONFIG" > "$OUT/sway.log" 2>&1 &
sleep 8
SWAY_SOCK=$(ls /run/user/1000/wayland-* 2>/dev/null | head -1 || true)
if [ -n "$SWAY_SOCK" ]; then
    export WAYLAND_DISPLAY="$(basename "$SWAY_SOCK")"
    ok "sway running on $WAYLAND_DISPLAY"
else
    bad "sway failed to start"
    tail -20 "$OUT/sway.log"
    finish_court FAIL "court" "wayland"
fi

# Evidence: the wlroots x11-backend window IS the compositor output; its
# geometry is the coordinate space the layer surface and the XWayland OSK
# live in. Query it once and use it for every click below. A failed query
# MUST fail the court — the old full-screen fallback silently mis-clicked
# the OSK (wrong centering/bottom edge) and turned a real failure into a
# confusing one.
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

# ── Layer-shell advertisement (capability probe, rule 13) ─────────────────
if sudo -u "$COURT_USER" env WAYLAND_DISPLAY="$WAYLAND_DISPLAY" XDG_RUNTIME_DIR=/run/user/1000 \
        python3 - <<'EOF' 2>/dev/null
import socket, os
# Probe via a tiny client asking the registry for zwlr_layer_shell_v1.
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
sock.connect(os.environ.get("WAYLAND_DISPLAY") and f"/run/user/1000/{os.environ['WAYLAND_DISPLAY']}")
# (full registry walk is done by the ferrokey UI itself; here we just
# confirm the socket accepts clients)
sock.close()
print("ok")
EOF
then
    ok "wayland socket reachable"
fi

start_recorder
start_target_wayland
start_ferrokeyd

# ── Ferrokey UI on native Wayland (layer shell) ───────────────────────────
# RUST_LOG=debug exposes the layer-surface pointer events in the evidence.
sudo -u "$COURT_USER" env WAYLAND_DISPLAY="$WAYLAND_DISPLAY" XDG_RUNTIME_DIR=/run/user/1000 \
    DISPLAY= RUST_LOG=debug "$PAYLOAD/bin/ferrokey" --config "$PAYLOAD/fixtures/ferrokey.yaml" \
    > "$OUT/ferrokey.log" 2>&1 &
FERROKEY_PID=$!
sleep 4
if kill -0 "$FERROKEY_PID" 2>/dev/null; then
    ok "ferrokey UI running on wayland"
else
    bad "ferrokey UI exited on wayland"
    cat "$OUT/ferrokey.log"
    finish_court FAIL "court" "wayland"
fi
if grep -q "wayland-layer-shell" "$OUT/ferrokey.log" 2>/dev/null; then
    ok "ferrokey selected the wayland-layer-shell backend"
else
    bad "ferrokey did not select layer-shell backend"
    cat "$OUT/ferrokey.log"
fi

# ── Focus the wayland target by clicking its window ───────────────────────
# A pointer click focuses the target (click-to-focus) — this is the initial
# focus, not a Ferrokey interaction. Click in the upper-middle of the output
# (the OSK layer surface occupies the bottom 342px), always on the target.
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove $((WINX + WINW / 2)) $((WINY + WINH / 2 - 150)) click 1
wait_focus 10

# ── Activate the OSK 'a' key ──────────────────────────────────────────────
# The sway x11-backend window IS the compositor output and the layer surface
# is anchored bottom (ferrokey-surface/wayland/layer_shell.rs) — but sway 1.7
# CENTERS a left|right-anchored surface that has a non-zero width
# (sway/desktop/layer_shell.c arrange_layer: box.x = bounds.x + (bounds.width
# / 2) - (box.width / 2)), so the OSK's left edge sits at
# (output_width - OSK width) / 2, not at 0. The click is a REAL pointer event
# through the X server into wlroots (wl_pointer) and down to the layer
# surface — the interactive half of the contract. The focus half is asserted
# by focus_before/focus_after: clicking the OSK must NOT move keyboard focus
# (keyboard_interactivity=none), so the target keeps it and receives the key.
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" a)
KX="${POS%,*}" KY="${POS#*,}"
OSK_W=$(awk '$1 == "width:" {print $2}' "$PAYLOAD/fixtures/ferrokey.yaml")
OSK_H=$(awk '$1 == "height:" {print $2}' "$PAYLOAD/fixtures/ferrokey.yaml")
TX=$((WINX + (WINW - OSK_W) / 2 + KX))
TY=$((WINY + WINH - OSK_H + KY))
focus_before
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool mousemove "$TX" "$TY" click 1
sleep 0.6

if grep -q '"event":"key","code":30,"down":true' "$EVENTS" 2>/dev/null; then
    ok "wayland target received KEY_A (kernel injection → compositor → app)"
else
    bad "wayland target did not receive KEY_A"
fi
focus_after
finish_court "court" "wayland"
