#!/usr/bin/env bash
# SEC.DEVLIFE.* — §73: device lifetime courts (also covers the implemented
# subset of §99: restart-safe authority, no ghost devices).
#
# One long-lived virtual keyboard per broker instance (§10): a UI
# connect/disconnect must not create or destroy kernel devices; a broker
# restart must remove the old device exactly once and create the new one
# exactly once.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

SOCK=/run/ferrokeyd/ferrokeyd.sock

device_count() {
    capture_devices
    grep -c 'Name="Ferrokey Virtual Keyboard"' "$OUT/devices.txt" || true
}

# ── start → exactly one device ──────────────────────────────────────────────
start_ferrokeyd
if [ "$(device_count)" = "1" ]; then
    ok "SEC.DEVLIFE.001 one device after broker start"
else
    bad "SEC.DEVLIFE.001 device count $(device_count) after start"
fi

# ── UI connect/disconnect cycles must not create or destroy devices ─────────
for _ in $(seq 1 5); do
    python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" \
        handshake key-down 30 key-up 30 release-all || true
done
if [ "$(device_count)" = "1" ]; then
    ok "SEC.DEVLIFE.002 device count stable across UI connect/disconnect cycles"
else
    bad "SEC.DEVLIFE.002 device count $(device_count) after cycles"
fi

# ── clean stop → device disappears ──────────────────────────────────────────
sudo kill -TERM "$FERROKEYD_PID" 2>/dev/null || true
sleep 2
if [ "$(device_count)" = "0" ]; then
    ok "SEC.DEVLIFE.003 device unregistered after clean stop"
else
    bad "SEC.DEVLIFE.003 device count $(device_count) after clean stop"
fi

# ── restart → old gone, new appears exactly once ────────────────────────────
start_ferrokeyd
if [ "$(device_count)" = "1" ]; then
    ok "SEC.DEVLIFE.004 exactly one device after restart"
else
    bad "SEC.DEVLIFE.004 device count $(device_count) after restart"
fi

# ── abrupt kill → device disappears, no ghost, restart safe (§74) ───────────
SERVE_PID=$(ferrokeyd_serve_pid)
if [ -n "$SERVE_PID" ]; then
    sudo kill -9 "$SERVE_PID" 2>/dev/null || true
    sleep 2
fi
if [ "$(device_count)" = "0" ]; then
    ok "SEC.DEVLIFE.005 device unregistered after SIGKILL"
else
    bad "SEC.DEVLIFE.005 device count $(device_count) after SIGKILL"
fi

start_ferrokeyd
if [ "$(device_count)" = "1" ] \
    && python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" handshake ping 5; then
    ok "SEC.DEVLIFE.006 restart after SIGKILL serves with one device"
else
    bad "SEC.DEVLIFE.006 restart after SIGKILL failed"
fi

finish_court "court" "device-lifetime"
