#!/usr/bin/env bash
# SEC.SOCKET.* — §26, §101: the daemon socket path must resist hijacking.
#
# bind_secure (§26) proves any existing endpoint is a Unix socket at the
# expected path owned appropriately before replacing it, and refuses to
# start (never deleting attacker-controlled data) when the target is a
# regular file, symlink, directory, device or foreign-owned socket.
# These gates attack the real daemon inside the VM — never the host (§55).
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

BIN="$PAYLOAD/bin/ferrokeyd"
CFG_TMPL="$PAYLOAD/fixtures/ferrokeyd.yaml"
SOCKDIR=/run/ferrokeyd
SOCK=$SOCKDIR/ferrokeyd.sock

# ── helpers ─────────────────────────────────────────────────────────────────
# make_config <socket-path> — a root-owned daemon config on a given socket.
# The redirect is root-side (tee) so a previous root-owned config can be
# overwritten from the unprivileged court user.
make_config() {
    local path="$1"
    sudo chown root:root "$CFG_TMPL"
    sudo chmod 0644 "$CFG_TMPL"
    sudo sed "s|socket_path: .*|socket_path: $path|" "$CFG_TMPL" \
        | sudo tee /tmp/fk-socket-config.yaml > /dev/null
    sudo chown root:root /tmp/fk-socket-config.yaml
    sudo chmod 0644 /tmp/fk-socket-config.yaml
    echo /tmp/fk-socket-config.yaml
}

# The supervisor must fail AND the sabotaged path must be untouched.
expect_refused() { # expect_refused <label> <check-command...>
    local label="$1"; shift
    local cfg rc=0
    cfg=$(make_config "$SOCK")
    sudo -u root env RUST_LOG=info "$BIN" start --config "$cfg" \
        >"$OUT/socket-hijack.log" 2>&1 || rc=$?
    if [ "$rc" -ne 0 ]; then
        if "$@"; then
            ok "$label (daemon refused, attacker file untouched)"
        else
            bad "$label daemon refused BUT the attacker file was modified"
            echo "--- daemon refused; state:"
            ls -la "$SOCK" 2>&1 | head -3 || true
            ls -la "$SOCKDIR" 2>&1 | head -3 || true
            tail -2 "$OUT/socket-hijack.log" || true
        fi
    else
        bad "$label daemon started despite the sabotage"
        sudo pkill -f "ferrokeyd start" 2>/dev/null || true
        sudo pkill -f "ferrokeyd serve" 2>/dev/null || true
    fi
}

# ── 1. regular file at the socket path (§26, §101) ──────────────────────────
sudo rm -f "$SOCK"
echo "attacker data" | sudo tee "$SOCK" > /dev/null
# The socket path is embedded literally (a shell variable is not exported
# into the sudo'd sh -c).
expect_refused "SEC.SOCKET.001 regular file at socket path" \
    sudo sh -c "grep -q 'attacker data' '$SOCK'"
sudo rm -f "$SOCK"

# ── 2. symlink at the socket path pointing at a victim file ─────────────────
sudo rm -f "$SOCK" /tmp/fk-victim
echo "victim" | sudo tee /tmp/fk-victim > /dev/null
sudo ln -s /tmp/fk-victim "$SOCK"
expect_refused "SEC.SOCKET.002 symlink at socket path" \
    sudo sh -c "[ -L '$SOCK' ] && grep -q victim /tmp/fk-victim"
sudo rm -f "$SOCK" /tmp/fk-victim

# ── 3. stale socket owned by a different user ───────────────────────────────
# The parent must stay ferrokeyd-owned (else the parent check fires first),
# so the court user briefly gets write access to create the foreign socket.
sudo rm -f "$SOCK"
sudo chmod 0777 "$SOCKDIR"
sudo -u court python3 - "$SOCK" <<'EOF' || true
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(sys.argv[1])
EOF
sudo chmod 0755 "$SOCKDIR"
expect_refused "SEC.SOCKET.003 foreign-owned stale socket" \
    sudo test -S "$SOCK"
sudo rm -f "$SOCK"

# ── 4. stale socket owned by the runtime user is proven + replaced ──────────
sudo rm -f "$SOCK"
# A stale socket owned by the ferrokeyd runtime user (bind as that user).
sudo -u ferrokeyd python3 - "$SOCK" <<'EOF' || true
import socket, sys
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.bind(sys.argv[1])
EOF
cfg=$(make_config "$SOCK")
sudo -u root env RUST_LOG=info "$BIN" start --config "$cfg" >"$OUT/socket-hijack-ok.log" 2>&1 &
FK_PID=$!
sleep 2
if [ -S "$SOCK" ] && python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" handshake ping 4; then
    ok "SEC.SOCKET.004 runtime-owned stale socket proven and replaced"
else
    bad "SEC.SOCKET.004 daemon failed over a replaceable stale socket"
    cat "$OUT/socket-hijack-ok.log"
fi
sudo kill -TERM "$FK_PID" 2>/dev/null || true
sleep 1
sudo pkill -f "ferrokeyd start" 2>/dev/null || true
sudo pkill -f "ferrokeyd serve" 2>/dev/null || true
sudo rm -f "$SOCK"

# ── 5. attacker-owned parent directory ──────────────────────────────────────
sudo chown court:court "$SOCKDIR"
sudo chmod 0777 "$SOCKDIR"
cfg=$(make_config "$SOCK")
rc=0
sudo -u root env RUST_LOG=info "$BIN" start --config "$cfg" >"$OUT/socket-hijack.log" 2>&1 || rc=$?
sudo chown ferrokeyd:ferrokeyd "$SOCKDIR"
sudo chmod 0755 "$SOCKDIR"
if [ "$rc" -ne 0 ]; then
    ok "SEC.SOCKET.005 attacker-owned parent directory refused"
else
    bad "SEC.SOCKET.005 daemon accepted an attacker-owned parent"
    sudo pkill -f "ferrokeyd start" 2>/dev/null || true
    sudo pkill -f "ferrokeyd serve" 2>/dev/null || true
fi
sudo rm -f "$SOCK"

# ── 6. normal start still works after all the sabotage (§105: fail closed) ──
start_ferrokeyd
python3 "$PAYLOAD/courts/fk-client.py" --socket "$SOCK" handshake key-down 30 key-up 30 release-all
if [ -S "$SOCK" ]; then
    ok "SEC.SOCKET.006 broker serves normally after hijack attempts"
else
    bad "SEC.SOCKET.006 broker not serving after hijack attempts"
fi

finish_court "court" "socket-hijack"
