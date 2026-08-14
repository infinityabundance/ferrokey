#!/usr/bin/env bash
# One-off debug: boot the RETAINED wayland-court failed overlay, refresh the
# payload, run the real court, then probe the LIVE guest state afterwards
# (the court leaves sway/Xorg/ferrokey running). ~2 minute boot instead of
# the ~25 minute full court VM.
#
#   debug-court.sh [wayland|xwayland]
set -e
COURT="${1:-wayland}"
STATE=/court/state
FAILED=$STATE/evidence/wayland/failed-overlay.qcow2
OVERLAY=$STATE/overlays/debug-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$FAILED" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debug*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debug
PUB=$(cat $STATE/keys/debug.pub)

cat > $STATE/seeds/debug-userdata.yaml <<EOF
#cloud-config
ssh_pwauth: false
disable_root: false
users:
  - name: court
    sudo: ALL=(ALL) NOPASSWD:ALL
    groups: [sudo]
    shell: /bin/bash
    lock_passwd: true
    ssh_authorized_keys:
      - "$PUB"
EOF
echo "instance-id: debug-$(date +%s%N)" > $STATE/seeds/debug-meta.yaml
cloud-localds $STATE/seeds/debug-seed.iso $STATE/seeds/debug-userdata.yaml $STATE/seeds/debug-meta.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debug-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22993-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debug.log" -monitor none -daemonize

for i in $(seq 1 120); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debug -p 22993 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debug -p 22993 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
# Refresh the payload so the debug always tests the CURRENT scripts: the
# binaries come from the most recent full payload build (courts/*.sh must
# come from the repo — the state payload may hold stale scripts).
BIN_SRC=$(ls -dt /repo/testing/evidence/*/payload/bin 2>/dev/null | head -1)
if [ -z "$BIN_SRC" ]; then
    echo "no built payload found under /repo/testing/evidence — run run-vm-court.sh first"
    exit 1
fi
rm -rf "$STATE/payload-debug"
mkdir -p "$STATE/payload-debug"
cp -r /repo/testing/courts "$STATE/payload-debug/courts"
cp -r /repo/testing/fixtures "$STATE/payload-debug/fixtures"
cp -r "$BIN_SRC" "$STATE/payload-debug/bin"
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debug -P 22993 -r "$STATE/payload-debug/." court@127.0.0.1:payload/ 2>/dev/null || true
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debug -p 22993 court@127.0.0.1 'chmod +x payload/bin/* payload/courts/*.sh 2>/dev/null || true'

echo "== running the real court: $COURT"
# The retained overlay has provisioned state but /run/ferrokeyd is a runtime
# dir (wiped at shutdown); the broker refuses to bind without it.
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debug -p 22993 court@127.0.0.1 \
  'sudo mkdir -p /run/ferrokeyd && sudo chown ferrokeyd:ferrokeyd /run/ferrokeyd && sudo chmod 0755 /run/ferrokeyd'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debug -p 22993 court@127.0.0.1 \
  "cd ~/payload && env RUN_ID=vm bash courts/$COURT/court.sh" || true
echo "== court finished (exit $?); probing the live guest"

scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debug -P 22993 \
  /repo/testing/vm/qemu/debug-probe.sh court@127.0.0.1:/tmp/debug-probe.sh 2>/dev/null || true
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debug -p 22993 court@127.0.0.1 'bash /tmp/debug-probe.sh' || true

qemu_pid=$(cat /court/state/qemu-debug.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug done"
