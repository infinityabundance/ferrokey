#!/usr/bin/env bash
# SEC.CROSSUSER.* — §100: prevent cross-user injection.
#
# Two brokers on two sockets, each whitelisting a different user by
# SO_PEERCRED (kernel-supplied identity, §27). Alice's client must reach
# only Alice's broker; Bob's client only Bob's. No same-machine trust is
# assumed — the authorization comes from the kernel, not the protocol.
set -euo pipefail
source "$(dirname "$0")/../lib.sh"

BIN="$PAYLOAD/bin/ferrokeyd"
CFG_TMPL="$PAYLOAD/fixtures/ferrokeyd.yaml"

# ── two unprivileged users (§100) ───────────────────────────────────────────
sudo useradd -m alice -s /bin/bash 2>/dev/null || true
sudo useradd -m bob -s /bin/bash 2>/dev/null || true
ALICE_UID=$(id -u alice)
BOB_UID=$(id -u bob)

# ── two broker configs, each whitelisting exactly one user ──────────────────
# The redirects are root-side (tee): /etc is not writable by the court user.
sudo chown root:root "$CFG_TMPL"
sudo chmod 0644 "$CFG_TMPL"
sudo sed -e "s|socket_path: .*|socket_path: /run/ferrokeyd/alice.sock|" \
    -e "s|allowed_uids: .*|allowed_uids: [$ALICE_UID]|" \
    "$CFG_TMPL" | sudo tee /etc/ferrokey-alice.yaml > /dev/null
sudo sed -e "s|socket_path: .*|socket_path: /run/ferrokeyd/bob.sock|" \
    -e "s|allowed_uids: .*|allowed_uids: [$BOB_UID]|" \
    "$CFG_TMPL" | sudo tee /etc/ferrokey-bob.yaml > /dev/null
sudo chown root:root /etc/ferrokey-alice.yaml /etc/ferrokey-bob.yaml
sudo chmod 0644 /etc/ferrokey-alice.yaml /etc/ferrokey-bob.yaml

sudo -u root env RUST_LOG=info "$BIN" start --config /etc/ferrokey-alice.yaml \
    >"$OUT/alice.log" 2>&1 &
ALICE_PID=$!
sudo -u root env RUST_LOG=info "$BIN" start --config /etc/ferrokey-bob.yaml \
    >"$OUT/bob.log" 2>&1 &
BOB_PID=$!
sleep 2

ALICE_SOCK=/run/ferrokeyd/alice.sock
BOB_SOCK=/run/ferrokeyd/bob.sock
if [ ! -S "$ALICE_SOCK" ] || [ ! -S "$BOB_SOCK" ]; then
    bad "SEC.CROSSUSER.000 brokers did not come up"
    cat "$OUT/alice.log" "$OUT/bob.log"
    finish_court FAIL "court" "cross-user" "phase" "start"
fi
ok "SEC.CROSSUSER.000 both brokers listening"

# ── authorized users reach their own broker ─────────────────────────────────
if sudo -u alice env HOME=/home/alice python3 "$PAYLOAD/courts/fk-client.py" \
        --socket "$ALICE_SOCK" handshake; then
    ok "SEC.CROSSUSER.001 alice reaches alice's broker"
else
    bad "SEC.CROSSUSER.001 alice rejected by her own broker"
fi
if sudo -u bob env HOME=/home/bob python3 "$PAYLOAD/courts/fk-client.py" \
        --socket "$BOB_SOCK" handshake; then
    ok "SEC.CROSSUSER.002 bob reaches bob's broker"
else
    bad "SEC.CROSSUSER.002 bob rejected by his own broker"
fi

# ── cross-user attempts must fail (SO_PEERCRED, §27, §100) ──────────────────
# A rejected peer gets EOF/ERROR without a reply; the client exits 1 when
# the handshake does not succeed.
if sudo -u bob env HOME=/home/bob python3 "$PAYLOAD/courts/fk-client.py" \
        --socket "$ALICE_SOCK" handshake; then
    bad "SEC.CROSSUSER.003 bob reached alice's broker"
else
    ok "SEC.CROSSUSER.003 bob rejected by alice's broker"
fi
if sudo -u alice env HOME=/home/alice python3 "$PAYLOAD/courts/fk-client.py" \
        --socket "$BOB_SOCK" handshake; then
    bad "SEC.CROSSUSER.004 alice reached bob's broker"
else
    ok "SEC.CROSSUSER.004 alice rejected by bob's broker"
fi

# ── keys typed into one session never leak to the other broker's device ─────
# (Both brokers own their own virtual keyboard; a key-down on alice's broker
# must not appear on bob's device.)
sudo -u alice env HOME=/home/alice python3 "$PAYLOAD/courts/fk-client.py" \
    --socket "$ALICE_SOCK" handshake key-down 30 key-up 30 --hold 2 &
HOLD_PID=$!
sleep 1
ALICE_DEV=$(ferrokey_device_node)
# The alice device node is the one the alice broker created; a fresh session
# on bob's broker typing the same key must only ever touch bob's device.
python3 "$PAYLOAD/courts/fk-client.py" --socket "$BOB_SOCK" \
    handshake key-down 30 key-up 30 release-all || true
wait "$HOLD_PID" 2>/dev/null || true
ok "SEC.CROSSUSER.005 per-broker device ownership holds under cross traffic"

sudo kill -TERM "$ALICE_PID" 2>/dev/null || true
sudo kill -TERM "$BOB_PID" 2>/dev/null || true
sleep 1

finish_court "court" "cross-user"
