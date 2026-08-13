#!/usr/bin/env bash
# Boot a court VM by DIRECT kernel boot (the KASAN/debug kernel, §66–§68).
#
# usage: boot-kernel.sh <overlay.qcow2> <seed.iso> <ssh-port> <kvm:0|1> <pidfile> <bzImage>
set -euo pipefail

OVERLAY="$1"; SEED="$2"; SSH_PORT="$3"; KVM="$4"; PIDFILE="$5"; BZIMAGE="$6"

ACCEL=""
if [ "$KVM" = "1" ] && [ -e /dev/kvm ]; then
    ACCEL="-machine accel=kvm -cpu host"
    echo "KVM acceleration: yes"
else
    echo "KVM acceleration: no (TCG software emulation)"
fi

# Direct boot, no initrd: virtio-blk + ext4 are built in. The cloud image's
# rootfs is /dev/vda1; cloud-init runs normally from the seed ISO (/dev/vdb).
APPEND="root=/dev/vda1 rootwait console=ttyS0 console=tty0 panic=-1"

qemu-system-x86_64 \
    $ACCEL \
    -m 2048 \
    -smp 2 \
    -kernel "$BZIMAGE" \
    -append "$APPEND" \
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
