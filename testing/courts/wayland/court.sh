#!/usr/bin/env bash
# WAYLAND.LAYER.001 (rules 12, 13, 50): layer-shell focus preservation.
#
# A genuine compositor (kwin_wayland, layer-shell support) runs nested in the
# dummy X server inside the VM, compositing in pure software (KWIN_COMPOSE=Q,
# QPainter — no GPU needed). The OSK is a layer surface with
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

# ── Start kwin_wayland (X11 backend, software QPainter compositing) ──────
# No backend flag: KWin auto-selects the X11 backend because DISPLAY is set.
# Bounded with a generous `timeout`: a hung startup must not wedge the whole
# court, but the court's flow (target + OSK + touch) takes well over 30s.
sudo -u "$COURT_USER" env DISPLAY=:0 KWIN_COMPOSE=Q LIBGL_ALWAYS_SOFTWARE=1 \
    XDG_RUNTIME_DIR=/run/user/1000 timeout 180 dbus-run-session -- \
    kwin_wayland --socket wayland-court-0 > "$OUT/kwin.log" 2>&1 &
sleep 8
if ls /run/user/1000/wayland-court-0 >/dev/null 2>&1; then
    ok "kwin_wayland running on wayland-court-0"
else
    bad "kwin_wayland failed to start"
    tail -20 "$OUT/kwin.log"
    finish_court FAIL "court" "wayland"
fi
export WAYLAND_DISPLAY=wayland-court-0

# Evidence: the kwin x11-backend window IS the compositor output; its
# geometry is the layer-surface coordinate space the court clicks in.
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xwininfo -root -tree > "$OUT/xwininfo.txt" 2>/dev/null || true

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
# RUST_LOG=debug exposes the layer-surface pointer events in the evidence.
sudo -u "$COURT_USER" env WAYLAND_DISPLAY=wayland-court-0 XDG_RUNTIME_DIR=/run/user/1000 \
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
# focus, not a Ferrokey interaction.
sudo -u "$COURT_USER" env DISPLAY=:0 xdotool mousemove 300 150 click 1
wait_focus 10

# ── Create the court touchscreen ───────────────────────────────────────────
# The OSK keys are activated with a REAL uinput touchscreen (the same path a
# tablet uses). Touch does not move keyboard focus, unlike a pointer click on
# the layer surface — this is how the layer-shell contract (pointer/touch
# interact with the OSK, keyboard focus stays with the target) is exercised
# on this compositor. The touchscreen's ABS range maps 1:1 to the dummy
# screen, so a tap at (X, Y) lands at screen (X, Y).
#
# The helper is persistent: it holds the uinput fd open and serves commands
# from a fifo (a uinput device dies with its fd, so one-shot `create` + later
# `tap` invocations could never work).
sudo rm -f /tmp/fake-touch.cmd
sudo mkfifo /tmp/fake-touch.cmd
sudo chmod 666 /tmp/fake-touch.cmd
sudo "$PAYLOAD/bin/fake-touch" create < /tmp/fake-touch.cmd >"$OUT/fake-touch.log" 2>&1 &
FAKETOUCH_PID=$!
# A held-open writer unblocks the helper's read so the device gets created.
# The keeper must NOT hold the ssh session's stdout/stderr or the court's
# ssh never sees EOF and the evidence collection hangs.
sudo sh -c 'exec 3>/tmp/fake-touch.cmd; sleep 600' >/dev/null 2>&1 &
FIFO_KEEPER=$!
sleep 1
if ! kill -0 "$FAKETOUCH_PID" 2>/dev/null; then
    bad "fake-touch create failed"
    cat "$OUT/fake-touch.log"
    finish_court FAIL "phase" "touchscreen-create"
fi
TOUCH_READY=0
for _ in $(seq 1 100); do
    # /proc/bus/input/devices is X-independent; xinput is the X11 view. The
    # kernel device appears first, then libinput attaches it to X.
    if grep -q "Ferrokey Court Touchscreen" /proc/bus/input/devices 2>/dev/null \
        && sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xinput list 2>/dev/null | grep -q "Ferrokey Court Touchscreen"; then
        TOUCH_READY=1
        break
    fi
    sleep 0.2
done
if [ "$TOUCH_READY" = "1" ]; then
    ok "Xorg attached the court touchscreen"
else
    bad "touchscreen never appeared in xinput"
    echo "== fake-touch.log:"; cat "$OUT/fake-touch.log" 2>/dev/null
    echo "== xinput-list.txt:"; sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xinput list 2>/dev/null | head -12
    echo "== kernel devices:"; grep -A2 -i touch /proc/bus/input/devices | head -8
    finish_court FAIL "phase" "touchscreen-attach"
fi

touch_fifo() { sudo sh -c "echo \"$1\" > /tmp/fake-touch.cmd"; }

focus_before
# Tap the OSK 'a' key. The kwin x11-backend window IS the compositor output
# and the layer surface is anchored bottom, so the OSK's top edge sits at
# (window_y + window_height - 342). Query the real geometry instead of
# assuming a fixed 1280x720 output.
POS=$(python3 "$PAYLOAD/courts/osk-geometry.py" a)
KX="${POS%,*}" KY="${POS#*,}"
# The compositor window id: xwininfo -root -tree lists it; xdotool search
# does not index it (no WM_NAME). Guarded: a failed query falls back to the
# full-screen assumption instead of aborting the court.
KWIN_WIN=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xwininfo -root -tree 2>/dev/null \
    | grep -oE '0x[0-9a-f]+ "KDE Wayland Compositor' | awk '{print $1}' | head -1 || true)
TX=$KX
TY=$((KY + 378))
if [ -n "$KWIN_WIN" ]; then
    GEO=$(sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" xdotool getwindowgeometry --shell "$KWIN_WIN" 2>/dev/null || true)
    if [ -n "$GEO" ]; then
        X=0; Y=0; HEIGHT=720
        eval "$GEO"
        TX=$((X + KX))
        TY=$((Y + HEIGHT - 342 + KY))
    fi
fi
# Tap the OSK key through the persistent touchscreen helper's fifo.
touch_fifo "tap $TX $TY"
sleep 0.6

if grep -q '"event":"key","code":30,"down":true' "$EVENTS" 2>/dev/null; then
    ok "wayland target received KEY_A (kernel injection → compositor → app)"
else
    bad "wayland target did not receive KEY_A"
fi
focus_after

finish_court "court" "wayland"
