#!/usr/bin/env bash
# One-off debug: strace uinput device creation in the guest.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debug2-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugkey2*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugkey2
PUB=$(cat $STATE/keys/debugkey2.pub)

cat > $STATE/seeds/debug2-userdata.yaml <<EOF
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
package_update: true
packages: [strace, evtest, jq]
runcmd:
  - [ bash, -c, "modprobe uinput; echo uinput > /etc/modules-load.d/ferrokey-uinput.conf" ]
EOF
cloud-localds $STATE/seeds/debug2-seed.iso $STATE/seeds/debug2-userdata.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debug2-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22998-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debug2.log" -monitor none -daemonize

for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugkey2 -p 22998 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey2 -p 22998 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
echo "=== push payload ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey2 -p 22998 court@127.0.0.1 'mkdir -p payload'
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey2 -P 22998 -r $STATE/payload/. court@127.0.0.1:payload/ 2>&1 | tail -3 || true
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey2 -p 22998 court@127.0.0.1 'ls -la payload/bin/ 2>&1 | head -5' || true
echo "=== strace virtinput device creation ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey2 -p 22998 court@127.0.0.1 'printf "key 30\n" | sudo strace -e trace=ioctl -f /home/court/payload/bin/ferrokey-test-virtinput 2>&1 | tail -40' || true
echo "=== device check ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey2 -p 22998 court@127.0.0.1 'sudo grep -A2 "Ferrokey" /proc/bus/input/devices || echo "no ferrokey device"' || true

echo "=== strace ferrokeyd device creation ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey2 -p 22998 court@127.0.0.1 'cd ~/payload && sudo bash -c "strace -f -e trace=ioctl,openat -o /tmp/fkd.strace ./bin/ferrokeyd --config fixtures/ferrokeyd.yaml >/tmp/fkd.log 2>&1 &" && sleep 1 && python3 courts/fk-client.py --socket /tmp/ferrokeyd.sock handshake 2>&1 | tail -2; sudo grep -E "EINVAL|ENOTTY|UI_DEV_CREATE|UI_DEV_SETUP|UI_SET_PHYS|UI_SET_KEYBIT.*= -1" /tmp/fkd.strace | head -20' || true

qemu_pid=$(cat /court/state/qemu-debug2.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug2 done"
