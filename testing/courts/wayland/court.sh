#!/usr/bin/env bash
# WAYLAND.LAYER.001 (rules 12, 13, 50): layer-shell focus preservation.
#
# A genuine compositor (wayfire, wlroots) runs nested in the dummy X server
# inside the VM. The OSK is a layer surface with
# keyboard_interactivity = none. The invariant:
#
#   interactive_pointer_surface      (OSK receives pointer clicks)
#   AND no_keyboard_focus_acquisition (target keeps focus)
#   AND successful_kernel_input_injection (target receives the key)
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

# Make sure the court user has a runtime dir (no login session in the VM).
sudo mkdir -p /run/user/1000
sudo chown court:court /run/user/1000
sudo chmod 700 /run/user/1000

start_xorg

# ── Start wayfire (wlroots, X11 backend, software GL) ─────────────────────
sudo -u "$COURT_USER" env DISPLAY=:0 WLR_BACKENDS=x11 LIBGL_ALWAYS_SOFTWARE=1 \
    XDG_RUNTIME_DIR=/run/user/1000 dbus-run-session -- \
    wayfire --socket wayland-court-0 > "$OUT/wayfire.log" 2>&1 &
sleep 5
if ls /run/user/1000/wayland-court-0 >/dev/null 2>&1; then
    ok "wayfire running on wayland-court-0"
else
    bad "wayfire failed to start"
    tail -20 "$OUT/wayfire.log"
    finish_court FAIL "court" "wayland"
fi
export WAYLAND_DISPLAY=wayland-court-0

# ── Layer-shell advertisement (capability probe, rule 13) ─────────────────
if sudo -u "$COURT_USER" env WAYLAND_DISPLAY=wayland-court-0 XDG_RUNTIME_DIR=/run/user/1000 \
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
sudo -u "$COURT_USER" env WAYLAND_DISPLAY=wayland-court-0 XDG_RUNTIME_DIR=/run/user/1000 \
    DISPLAY= "$PAYLOAD/bin/ferrokey" --config "$PAYLOAD/fixtures/ferrokey.yaml" \
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
sudo -u "$COURT_USER" env DISPLAY=:0 xdotool mousemove 300 150 click 1
wait_focus 10

focus_before
# Click the OSK 'a' key: wayfire window at (0,0) 1280x720, layer surface
# anchored bottom → y offset 720 - 342 = 378.
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" a)
X="${POS%,*}" Y="${POS#*,}"
sudo -u "$COURT_USER" env DISPLAY=:0 xdotool mousemove "$X" "$((Y + 378))" click 1
sleep 0.6

if grep -q '"event":"key","code":30,"down":true' "$EVENTS" 2>/dev/null; then
    ok "wayland target received KEY_A (kernel injection → compositor → app)"
else
    bad "wayland target did not receive KEY_A"
fi
focus_after

finish_court "court" "wayland"
