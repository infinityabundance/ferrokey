#!/usr/bin/env bash
# BACKEND.SELECTION.001 (§65/§66 of the addendum): the surface backend is
# selected by CAPABILITY detection — deterministically, never by compositor
# name — across the fixture matrix:
#
#   fixture                          expected selection
#   headless                         none
#   X11-only (Xorg + openbox)        x11-no-input
#   Wayland + layer-shell (sway)     wayland-layer-shell
#   Wayland − layer-shell + X11      x11-no-input (XWayland fallback)
#   Wayland − layer-shell − X11      wayland-degraded
#
# Every assertion reads the app's OWN startup log line ("surface backend:
# <name> (<detail>)") — the real selection path — and the fallback cases
# additionally require the *rejection reason* to be present in the detail
# (§66: reasons are logged, never silent).
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

mkdir -p /tmp/court-runtime
sudo chmod 777 /tmp/court-runtime 2>/dev/null || true

# ── the selection assertion helper ──────────────────────────────────────────
# Run the real app under a session environment and assert its own decision.
# $1 = fixture id, $2 = expected backend name, $3 = required detail needle
# ("" = any), then KEY=VALUE pairs forming the session environment.
assert_backend() {
    local id="$1" expected="$2" needle="$3"
    shift 3
    local logfile="$OUT/backend-$id.log"
    : > "$logfile"
    env RUST_LOG=info "$@" \
        timeout 25 "$PAYLOAD/bin/ferrokey" --config "$PAYLOAD/fixtures/ferrokey.yaml" \
        >"$logfile" 2>&1 &
    local pid=$!
    local line=""
    for _ in $(seq 1 60); do
        line=$(grep -m1 "surface backend:" "$logfile" 2>/dev/null || true)
        [ -n "$line" ] && break
        sleep 0.25
    done
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    if echo "$line" | grep -q "surface backend: $expected"; then
        if [ -n "$needle" ] && ! echo "$line" | grep -q "$needle"; then
            bad "$id: selected '$expected' but the detail lacks '$needle': $line"
            return
        fi
        ok "$id: app selected '$expected' — ${line#*backend: }"
    else
        bad "$id: expected backend '$expected', observed: ${line:-NO LOG LINE}"
        tail -8 "$logfile" 2>/dev/null || true
    fi
}

# ── 1. headless ─────────────────────────────────────────────────────────────
assert_backend "headless" "none" "" \
    DISPLAY= WAYLAND_DISPLAY= XDG_RUNTIME_DIR=/tmp/court-runtime

# ── 2. X11-only (Xorg + openbox) ────────────────────────────────────────────
start_xorg
assert_backend "x11-only" "x11-no-input" "X11 session on" \
    DISPLAY="$DISPLAY" WAYLAND_DISPLAY= XDG_RUNTIME_DIR=/tmp/court-runtime

# ── 3. Wayland + layer-shell (sway / wlroots, the reference compositor) ─────
# sway is the window manager here (COURT_NO_WM: no openbox — the wlroots
# x11-backend window must stay unmanaged at (0,0)).
export COURT_NO_WM=1
sudo mkdir -p /run/user/1000
sudo chown "$COURT_USER":"$COURT_USER" /run/user/1000
sudo chmod 700 /run/user/1000
COMPOSITOR_CONFIG="$OUT/sway-court.config"
cat > "$COMPOSITOR_CONFIG" <<'EOF'
xwayland enable
output "*" {
    bg #202020 solid_color
}
EOF
sudo -u "$COURT_USER" env DISPLAY="$DISPLAY" WLR_BACKENDS=x11 WLR_RENDERER=pixman \
    XDG_RUNTIME_DIR=/run/user/1000 \
    timeout 150 sway -c "$COMPOSITOR_CONFIG" > "$OUT/sway.log" 2>&1 &
SWAY_PID=$!
sleep 8
SWAY_SOCK=$(ls /run/user/1000/wayland-* 2>/dev/null | head -1 || true)
if [ -n "$SWAY_SOCK" ]; then
    ok "sway running on $(basename "$SWAY_SOCK") (layer-shell available)"
else
    bad "sway failed to start"
    tail -20 "$OUT/sway.log"
    finish_court FAIL "phase" "sway"
fi
assert_backend "wl-layershell" "wayland-layer-shell" "zwlr_layer_shell_v1" \
    WAYLAND_DISPLAY="$(basename "$SWAY_SOCK")" DISPLAY="$DISPLAY" XDG_RUNTIME_DIR=/run/user/1000

# ── 4+5. the mini-compositor (Wayland WITHOUT layer-shell) ──────────────────
kill "$SWAY_PID" 2>/dev/null || true
sleep 1
# The ssh session sets XDG_RUNTIME_DIR=/run/user/1000 (pam_systemd); pin the
# compositor to the SAME runtime dir the app connects through so the socket
# the court checks is the socket the app probes.
env XDG_RUNTIME_DIR=/tmp/court-runtime \
    "$PAYLOAD/bin/ferrokey-test-mini-compositor" ferrokey-mini \
    > "$OUT/mini-compositor.log" 2>&1 &
MINI_PID=$!
sleep 1
if [ -S /tmp/court-runtime/ferrokey-mini ]; then
    ok "mini-compositor listening on ferrokey-mini (no layer-shell advertised)"
else
    bad "mini-compositor did not start"
    tail -5 "$OUT/mini-compositor.log"
    finish_court FAIL "phase" "mini-compositor"
fi

# 4: Wayland without layer-shell, but an X display exists → XWayland fallback.
assert_backend "wl-nols-xwayland" "x11-no-input" "without layer-shell" \
    WAYLAND_DISPLAY=ferrokey-mini DISPLAY="$DISPLAY" XDG_RUNTIME_DIR=/tmp/court-runtime

# 5: Wayland without layer-shell and no X display → explicit degraded mode.
assert_backend "wl-nols-degraded" "wayland-degraded" "without layer-shell" \
    WAYLAND_DISPLAY=ferrokey-mini DISPLAY= XDG_RUNTIME_DIR=/tmp/court-runtime

kill "$MINI_PID" 2>/dev/null || true

finish_court "court" "backend-selection"
