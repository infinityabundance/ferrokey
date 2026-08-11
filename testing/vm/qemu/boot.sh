#!/usr/bin/env bash
# Boot a disposable court VM (run INSIDE the oracle container).
#
# usage: boot.sh <overlay.qcow2> <seed.iso> <ssh-port> <kvm:0|1> <pidfile>
set -euo pipefail

OVERLAY="$1"; SEED="$2"; SSH_PORT="$3"; KVM="$4"; PIDFILE="$5"

ACCEL=""
if [ "$KVM" = "1" ] && [ -e /dev/kvm ]; then
    ACCEL="-machine accel=kvm -cpu host"
    echo "KVM acceleration: yes"
else
    echo "KVM acceleration: no (TCG software emulation)"
fi

qemu-system-x86_64 \
    $ACCEL \
    -m 2048 \
    -smp 2 \
    -drive "file=$OVERLAY,format=qcow2,if=virtio" \
    -drive "file=$SEED,format=raw,if=virtio" \
    -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:$SSH_PORT-:22" \
    -device virtio-net-pci,netdev=n1 \
    -display none \
    -serial "file:/court/state/logs/qemu-$SSH_PORT.log" \
    -monitor none \
    -pidfile "$PIDFILE" \
    -daemonize

echo "VM booted (pid $(cat "$PIDFILE"), ssh port $SSH_PORT)"
