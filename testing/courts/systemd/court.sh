#!/usr/bin/env bash
# SEC.SYSTEMD.001 — the hardened systemd unit (§38–§40) installs and the
# serving process satisfies the same privilege invariants as the direct
# launch path. Defense in depth: the unit must not silently disable the
# internal hardening.
#
# Gates:
#   SEC.SYSTEMD.001  unit installs and the broker comes up under systemd
#   SEC.SYSTEMD.002  serving process euid != 0 (§57)
#   SEC.SYSTEMD.003  capability sets empty + bounding set empty (§58)
#   SEC.SYSTEMD.004  NoNewPrivs=1 (§59)
#   SEC.SYSTEMD.005  seccomp mode 2 (§32)
#   SEC.SYSTEMD.006  AF_UNIX listener reachable by the authorized user
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

SOCK=/run/ferrokeyd/ferrokeyd.sock

# ── install the unit + production config (§38, §45) ────────────────────────
if [ ! -f "$PAYLOAD/PACKAGING/ferrokeyd.service" ]; then
    bad "SEC.SYSTEMD.000 packaging unit missing from payload"
    finish_court FAIL "court" "systemd" "phase" "payload"
fi

# The unit's ExecStart is /usr/bin/ferrokeyd: install the real binary there.
sudo install -m 0755 "$PAYLOAD/bin/ferrokeyd" /usr/bin/ferrokeyd

sudo install -m 0644 "$PAYLOAD/PACKAGING/ferrokeyd.service" /etc/systemd/system/ferrokeyd.service
# Production config: root-owned, 0644. The fixture's allowed_uids must match
# the court user (uid 1000) so the broker is usable.
sudo mkdir -p /etc/ferrokey
sudo cp "$PAYLOAD/fixtures/ferrokeyd.yaml" /etc/ferrokey/ferrokeyd.yaml
sudo chown root:root /etc/ferrokey/ferrokeyd.yaml
sudo chmod 0644 /etc/ferrokey/ferrokeyd.yaml

sudo systemctl daemon-reload
sudo systemctl start ferrokeyd || {
    bad "SEC.SYSTEMD.001 systemd unit failed to start"
    sudo journalctl -u ferrokeyd --no-pager -n 40 2>/dev/null || true
    finish_court FAIL "court" "systemd" "phase" "unit-start"
}

# Wait for the listener.
for _ in $(seq 1 50); do
    [ -S "$SOCK" ] && break
    sleep 0.2
done
if [ -S "$SOCK" ]; then
    ok "SEC.SYSTEMD.001 broker listening under systemd"
else
    bad "SEC.SYSTEMD.001 no listener after systemd start"
    sudo journalctl -u ferrokeyd --no-pager -n 40 2>/dev/null || true
    finish_court FAIL "court" "systemd" "phase" "listener"
fi

# ── the serving process under systemd ──────────────────────────────────────
SERVE_PID=$(ferrokeyd_serve_pid)
if [ -z "$SERVE_PID" ]; then
    bad "SEC.SYSTEMD.002 no serve process under systemd"
    finish_court FAIL "court" "systemd" "phase" "serve-pid"
fi

STATUS=/proc/$SERVE_PID/status
BROKER_EUID=$(awk '/^Uid:/{print $2}' "$STATUS")
if [ "$BROKER_EUID" != "0" ]; then
    ok "SEC.SYSTEMD.002 euid != 0 (euid=$BROKER_EUID)"
else
    bad "SEC.SYSTEMD.002 euid is 0"
fi

CAPINH=$(awk '/^CapInh:/{print $2}' "$STATUS")
CAPPRM=$(awk '/^CapPrm:/{print $2}' "$STATUS")
CAPEFF=$(awk '/^CapEff:/{print $2}' "$STATUS")
CAPAMB=$(awk '/^CapAmb:/{print $2}' "$STATUS")
CAPBND=$(awk '/^CapBnd:/{print $2}' "$STATUS")
if [ "$CAPINH" = "0000000000000000" ] && [ "$CAPPRM" = "0000000000000000" ] \
    && [ "$CAPEFF" = "0000000000000000" ] && [ "$CAPAMB" = "0000000000000000" ]; then
    ok "SEC.SYSTEMD.003 capability sets empty"
else
    bad "SEC.SYSTEMD.003 capability sets NOT empty (Inh=$CAPINH Prm=$CAPPRM Eff=$CAPEFF Amb=$CAPAMB)"
fi
if [ "$CAPBND" = "0000000000000000" ]; then
    ok "SEC.SYSTEMD.003b bounding set empty"
else
    bad "SEC.SYSTEMD.003b bounding set NOT empty (CapBnd=$CAPBND)"
fi

NNP=$(awk '/^NoNewPrivs:/{print $2}' "$STATUS")
if [ "$NNP" = "1" ]; then
    ok "SEC.SYSTEMD.004 no_new_privs set"
else
    bad "SEC.SYSTEMD.004 no_new_privs NOT set"
fi

SECCMODE=$(awk '/^Seccomp:/{print $2}' "$STATUS")
if [ "$SECCMODE" = "2" ]; then
    ok "SEC.SYSTEMD.005 seccomp mode 2 active"
else
    bad "SEC.SYSTEMD.005 seccomp NOT active (Seccomp=$SECCMODE)"
fi

# The unit itself must carry the hardening directives (§38) — systemd-analyze
# verify is run, not just grepped: a broken unit fails the court.
if sudo systemd-analyze verify /etc/systemd/system/ferrokeyd.service >"$OUT/systemd-analyze.log" 2>&1; then
    ok "SEC.SYSTEMD.006 systemd-analyze verify clean"
else
    bad "SEC.SYSTEMD.006 systemd-analyze verify FAILED"
    cat "$OUT/systemd-analyze.log"
fi

# The authorized desktop user can drive the keyboard through the unit-served
# broker (§27, §114).
if python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" \
        handshake key-down 30 key-up 30 release-all; then
    ok "SEC.SYSTEMD.007 authorized user can command the keyboard"
else
    bad "SEC.SYSTEMD.007 authorized user rejected"
fi

# Record the effective hardening of the unit (systemd may silently ignore
# directives it does not know on older versions — §40).
{
    echo "=== systemd version ==="
    systemd --version 2>/dev/null | head -2 || true
    echo "=== effective unit ==="
    sudo systemctl show ferrokeyd --property=NoNewPrivileges,CapabilityBoundingSet,AmbientCapabilities,PrivateTmp,ProtectSystem,ProtectHome,ProtectKernelTunables,ProtectKernelModules,ProtectKernelLogs,ProtectControlGroups,RestrictNamespaces,RestrictSUIDSGID,LockPersonality,MemoryDenyWriteExecute,RestrictRealtime,RestrictAddressFamilies,SystemCallArchitectures,DevicePolicy,DeviceAllow 2>/dev/null
    echo "=== security-status ==="
    "$PAYLOAD/bin/ferrokeyd" security-status --pid "$SERVE_PID" 2>/dev/null || true
} > "$OUT/systemd-effective.txt"

# Verify the recorded directives are ACTIVE (not just present in the file).
if sudo systemctl show ferrokeyd --property=NoNewPrivileges 2>/dev/null | grep -q "yes"; then
    ok "SEC.SYSTEMD.008 NoNewPrivileges effective"
else
    bad "SEC.SYSTEMD.008 NoNewPrivileges not effective"
fi
if sudo systemctl show ferrokeyd --property=RestrictAddressFamilies 2>/dev/null | grep -q "AF_UNIX"; then
    ok "SEC.SYSTEMD.009 RestrictAddressFamilies=AF_UNIX effective"
else
    bad "SEC.SYSTEMD.009 RestrictAddressFamilies not effective"
fi
if sudo systemctl show ferrokeyd --property=DevicePolicy 2>/dev/null | grep -q "closed"; then
    ok "SEC.SYSTEMD.010 DevicePolicy=closed effective"
else
    bad "SEC.SYSTEMD.010 DevicePolicy not closed"
fi

# ── §102 filesystem confinement ─────────────────────────────────────────────
# The unit-level confinement (ProtectSystem=strict, ProtectHome=yes) plus the
# in-process seccomp openat denial (proven by the kernel-security sandbox
# probe) mean the serving process cannot write outside the runtime state.
if sudo systemctl show ferrokeyd --property=ProtectSystem 2>/dev/null | grep -q "strict"; then
    ok "SEC.SYSTEMD.014 ProtectSystem=strict effective"
else
    bad "SEC.SYSTEMD.014 ProtectSystem not strict"
fi
if sudo systemctl show ferrokeyd --property=ProtectHome 2>/dev/null | grep -q "yes"; then
    ok "SEC.SYSTEMD.015 ProtectHome=yes effective"
else
    bad "SEC.SYSTEMD.015 ProtectHome not effective"
fi

# Clean shutdown via the unit (the broker exits cleanly on SIGTERM). The
# socket *file* may remain after the listener closes — Unix sockets persist
# until unlinked — so the gates assert the listener is really closed and
# that a restart proves-and-replaces the stale socket (§26, §101).
sudo systemctl stop ferrokeyd
sleep 1
if ! sudo systemctl is-active --quiet ferrokeyd && [ -z "$(ferrokeyd_serve_pid)" ]; then
    ok "SEC.SYSTEMD.011 broker stopped cleanly (unit inactive, no serve process)"
else
    bad "SEC.SYSTEMD.011 broker still active after stop"
fi
if python3 - "$SOCK" <<'EOF' 2>/dev/null
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.settimeout(1)
try:
    s.connect(sys.argv[1])
    sys.exit(1)  # connected: listener still alive
except OSError:
    sys.exit(0)  # refused: listener closed
EOF
then
    ok "SEC.SYSTEMD.012 listener closed after stop (connect refused)"
else
    bad "SEC.SYSTEMD.012 listener still accepting after stop"
fi
# Restart must rebind cleanly over the stale socket file (§101). The stale
# file exists before the daemon rebinds, so wait for a real connect.
sudo systemctl start ferrokeyd
for _ in $(seq 1 50); do
    if python3 -c "import socket,sys; s=socket.socket(socket.AF_UNIX, socket.SOCK_STREAM); s.settimeout(0.5); s.connect('$SOCK')" 2>/dev/null; then
        break
    fi
    sleep 0.2
done
if python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" handshake ping 3; then
    ok "SEC.SYSTEMD.013 restart rebinds cleanly over the stale socket"
else
    bad "SEC.SYSTEMD.013 restart failed over the stale socket"
fi
sudo systemctl stop ferrokeyd
sleep 1

finish_court "court" "systemd"
