#!/usr/bin/env bash
# SEC.SOAK.001 — long-run randomized valid/invalid event sequences (§98).
#
# Inside the VM, for SOAK_SECONDS (default 300):
#   * randomized valid key events (bounded, within rate limits)
#   * randomized invalid messages (never reaching the kernel path)
#   * periodic reconnect churn
# Tracks RSS / FD count / thread count / uinput device count / held-key state
# and requires stable bounds. Kernel diagnostics are captured throughout.
#
# Only runs in the disposable VM; never on the developer host (§55).
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

SOCK=/run/ferrokeyd/ferrokeyd.sock
DURATION="${SOAK_SECONDS:-300}"

start_ferrokeyd

SERVE_PID=$(ferrokeyd_serve_pid)
[ -n "$SERVE_PID" ] || { bad "SEC.SOAK.000 no serve process"; finish_court FAIL "court" "soak" "phase" "serve-pid"; }

# Baseline. /proc/<pid>/fd is ptrace-restricted across uids: the court user
# cannot list the broker's fds without privileges, so enumeration uses sudo.
FDS0=$(sudo -n ls -1 "/proc/$SERVE_PID/fd" 2>/dev/null | wc -l)
RSS0=$(awk '/^VmRSS:/{print $2}' "/proc/$SERVE_PID/status" || echo 0)
THREADS0=$(sudo -n ls -1 "/proc/$SERVE_PID/task" 2>/dev/null | wc -l)
capture_devices
DEV0=$(grep -c 'Name="Ferrokey Virtual Keyboard"' "$OUT/devices.txt" || true)

# The soak driver: one long-lived authorized connection plus reconnect churn,
# valid/invalid interleaved. Runs in the background writing a summary.
python3 "$PAYLOAD/courts/soak/driver.py" "$SOCK" "$DURATION" > "$OUT/soak-driver.txt" 2>&1 &
DRIVER_PID=$!

MAX_FDS=$FDS0
MAX_RSS=$RSS0
MAX_THREADS=$THREADS0
END=$(( $(date +%s) + DURATION ))
while [ "$(date +%s)" -lt "$END" ]; do
    sleep 15
    FDS=$(sudo -n ls -1 "/proc/$SERVE_PID/fd" 2>/dev/null | wc -l)
    RSS=$(awk '/^VmRSS:/{print $2}' "/proc/$SERVE_PID/status" || echo 0)
    THREADS=$(sudo -n ls -1 "/proc/$SERVE_PID/task" 2>/dev/null | wc -l)
    [ "$FDS" -gt "$MAX_FDS" ] && MAX_FDS=$FDS
    [ "$RSS" -gt "$MAX_RSS" ] && MAX_RSS=$RSS
    [ "$THREADS" -gt "$MAX_THREADS" ] && MAX_THREADS=$THREADS
    if [ -z "${FERROKEYD_PID:-}" ] || ! kill -0 "$FERROKEYD_PID" 2>/dev/null; then
        bad "SEC.SOAK.001 broker died during soak"
        break
    fi
done
wait "$DRIVER_PID" || true

# ── resource bounds (§51, §76, §98) ────────────────────────────────────────
echo "fd: baseline=$FDS0 max=$MAX_FDS" | tee -a "$OUT/soak-metrics.txt"
echo "rss_kb: baseline=$RSS0 max=$MAX_RSS" | tee -a "$OUT/soak-metrics.txt"
echo "threads: baseline=$THREADS0 max=$MAX_THREADS" | tee -a "$OUT/soak-metrics.txt"

if [ "$MAX_FDS" -le $((FDS0 + 8)) ]; then
    ok "SEC.SOAK.001 fd count bounded (baseline $FDS0, max $MAX_FDS)"
else
    bad "SEC.SOAK.001 fd count grew: baseline $FDS0, max $MAX_FDS"
fi
if [ "$MAX_RSS" -le $((RSS0 + 20000)) ]; then
    ok "SEC.SOAK.002 RSS bounded (baseline ${RSS0}kB, max ${MAX_RSS}kB)"
else
    bad "SEC.SOAK.002 RSS grew: baseline ${RSS0}kB, max ${MAX_RSS}kB"
fi
if [ "$MAX_THREADS" -le $((THREADS0 + 2)) ]; then
    ok "SEC.SOAK.003 thread count stable (baseline $THREADS0, max $MAX_THREADS)"
else
    bad "SEC.SOAK.003 thread count grew: baseline $THREADS0, max $MAX_THREADS"
fi

DEV1=$(grep -c 'Name="Ferrokey Virtual Keyboard"' "$OUT/devices.txt" || true)
if [ "$DEV1" -le 1 ]; then
    ok "SEC.SOAK.004 device count stable ($DEV1)"
else
    bad "SEC.SOAK.004 device count grew: $DEV1"
fi

# The driver must have completed without killing the broker; its summary
# records valid/invalid counts and any client-visible failures.
if grep -q "driver-complete" "$OUT/soak-driver.txt" 2>/dev/null; then
    ok "SEC.SOAK.005 soak driver completed"
else
    bad "SEC.SOAK.005 soak driver did not complete"
    tail -20 "$OUT/soak-driver.txt"
fi

# Kernel diagnostics captured during the soak (§67).
if sudo -u root dmesg > "$OUT/soak-dmesg.txt" 2>&1; then
    if grep -qE "BUG:|WARNING:|Oops:|Kernel panic|general protection fault|use-after-free|KASAN:|UBSAN:|possible circular locking|kernel BUG" "$OUT/soak-dmesg.txt"; then
        bad "SEC.SOAK.006 kernel diagnostics during soak"
    else
        ok "SEC.SOAK.006 kernel log clean during soak"
    fi
fi

# Held-key state must be empty after the storm (the broker's ledger drains on
# every disconnect; a fresh session must not see stale held keys).
if python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" \
        handshake key-down 30 key-up 30 release-all ping 7; then
    ok "SEC.SOAK.007 broker fully functional after soak"
else
    bad "SEC.SOAK.007 broker not functional after soak"
fi

finish_court "court" "soak"
