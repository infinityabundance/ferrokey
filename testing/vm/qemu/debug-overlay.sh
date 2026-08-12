#!/usr/bin/env bash
# One-off debug: boot the RETAINED failed overlay from the x11 court and
# inspect what the target/recorder actually did.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
FAILED=$STATE/evidence/x11/failed-overlay.qcow2
OVERLAY=$STATE/overlays/debug4-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$FAILED" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugkey4*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugkey4

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22996-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debug4.log" -monitor none -daemonize

for i in $(seq 1 100); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugkey4 -p 22996 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey4 -p 22996 court@127.0.0.1 'echo "--- court-output ---"; ls -la court-output/ 2>&1; echo "--- events.log ---"; cat court-output/events.log 2>/dev/null; echo "--- target.log ---"; cat court-output/target.log 2>/dev/null; echo "--- ps ---"; pgrep -af "ferrokey" || true; echo "--- socket ---"; ls -la /tmp/ferrokey-test-target.sock /tmp/ferrokeyd.sock 2>&1' || true

qemu_pid=$(cat /court/state/qemu-debug4.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug4 done"
