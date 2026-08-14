#!/usr/bin/env bash
# Debug: boot the retained session-lifetime failed overlay and test the
# cgroup move + session authorization manually.
set -e
STATE=/court/state
FAILED=$STATE/evidence/session-lifetime/failed-overlay.qcow2
OVERLAY=$STATE/overlays/dbgsess-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$FAILED" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/dbgsess*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/dbgsess
PUB=$(cat $STATE/keys/dbgsess.pub)

cat > $STATE/seeds/dbgsess.yaml <<EOF
#cloud-config
users:
  - name: court
    sudo: ALL=(ALL) NOPASSWD:ALL
    groups: [sudo]
    shell: /bin/bash
    lock_passwd: true
    ssh_authorized_keys:
      - "$PUB"
EOF
echo "instance-id: dbgsess-$(date +%s%N)" > $STATE/seeds/dbgsess-meta.yaml
cloud-localds $STATE/seeds/dbgsess-seed.iso $STATE/seeds/dbgsess.yaml $STATE/seeds/dbgsess-meta.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/dbgsess-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22994-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-dbgsess.log" -monitor none -daemonize

for i in $(seq 1 120); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/dbgsess -p 22994 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/dbgsess -p 22994 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'

ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/dbgsess -p 22994 court@127.0.0.1 'bash -s' <<'GUEST'
set -x
sudo bash -c '
  mount | grep -i cgroup
  ls /sys/fs/cgroup/ | head
  mkdir -p /sys/fs/cgroup/session-99.scope && echo MKOK
  ls -ld /sys/fs/cgroup/session-99.scope
  echo $$ > /sys/fs/cgroup/session-99.scope/cgroup.procs && echo SELFOK
  cat /proc/self/cgroup
  echo "--- now as the court user via su:"
  echo $$ > /sys/fs/cgroup/session-99.scope/cgroup.procs
  exec su -s /bin/sh court -c "cat /proc/self/cgroup"
'
GUEST
echo "== guest probe done"
qemu_pid=$(cat /court/state/qemu-dbgsess.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo done
