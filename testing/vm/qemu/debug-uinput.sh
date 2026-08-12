#!/usr/bin/env bash
# One-off debug: boot the court base image and inspect uinput availability.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debug-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugkey
PUB=$(cat $STATE/keys/debugkey.pub)

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
package_update: false
EOF
cloud-localds $STATE/seeds/debug-seed.iso $STATE/seeds/debug-userdata.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debug-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22999-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debug.log" -monitor none -daemonize

for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugkey -p 22999 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
echo "=== guest diagnostics ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey -p 22999 court@127.0.0.1 'sudo bash -c "
echo --- uname: \$(uname -r)
echo --- modprobe attempt:
modprobe uinput; echo MODPROBE_EXIT=\$?
echo --- lsmod:
lsmod | grep uinput || echo \"uinput not loaded\"
echo --- module file:
find /lib/modules -maxdepth 4 -name \"uinput*\" 2>/dev/null || echo \"no module file found\"
echo --- device:
ls -la /dev/uinput 2>&1 || true
echo --- os:
grep PRETTY_NAME /etc/os-release
"'

qemu_pid=$(cat /court/state/qemu-debug.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug done"
