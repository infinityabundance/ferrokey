#!/usr/bin/env bash
# One-off debug: start Xorg in the guest and run the raw X11 court target.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debug3-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugkey3*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugkey3
PUB=$(cat $STATE/keys/debugkey3.pub)

cat > $STATE/seeds/debug3-userdata.yaml <<EOF
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
packages: [xserver-xorg-video-dummy, xserver-xorg-core, xinit, x11-utils, xdotool, x11-xserver-utils, evtest, strace, jq, procps, python3]
write_files:
  - path: /etc/X11/Xwrapper.config
    content: |
      allowed_users=anybody
      needs_root_rights=yes
  - path: /etc/X11/xorg.conf.d/99-ferrokey-dummy.conf
    content: |
      Section "Device"
          Identifier  "DummyDevice"
          Driver      "dummy"
          VideoRam    65536
      EndSection
      Section "Screen"
          Identifier  "DummyScreen"
          Device      "DummyDevice"
          DefaultDepth 24
          SubSection "Display"
              Depth 24
              Modes "1280x720"
          EndSubSection
      EndSection
EOF
cloud-localds $STATE/seeds/debug3-seed.iso $STATE/seeds/debug3-userdata.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debug3-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22997-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debug3.log" -monitor none -daemonize

for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugkey3 -p 22997 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey3 -p 22997 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey3 -p 22997 court@127.0.0.1 'mkdir -p payload'
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey3 -P 22997 -r $STATE/payload/bin court@127.0.0.1:payload/ 2>/dev/null || true
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey3 -p 22997 court@127.0.0.1 'chmod +x payload/bin/*'

echo "=== start Xorg + openbox ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey3 -p 22997 court@127.0.0.1 'Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 & sleep 3; DISPLAY=:0 xdpyinfo >/dev/null 2>&1 && echo "X OK" || tail -5 /tmp/xorg.log; DISPLAY=:0 openbox >/tmp/openbox.log 2>&1 & sleep 2; echo started' || true

echo "=== run x11 target + recorder ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey3 -p 22997 court@127.0.0.1 'timeout 8 env DISPLAY=:0 TARGET_SOCKET=/tmp/ferrokey-test-target.sock ./payload/bin/ferrokey-test-target-x11 >/tmp/target.log 2>&1 & sleep 1; ls -la payload/courts/ 2>&1 | head -5; python3 payload/courts/recv-events.py /tmp/ferrokey-test-target.sock > /tmp/events.log 2>/tmp/recv.err & sleep 3; echo "--- events.log ---"; cat /tmp/events.log; echo "--- recv.err ---"; cat /tmp/recv.err; echo "--- target.log ---"; cat /tmp/target.log; kill %1 2>/dev/null || true' || true

qemu_pid=$(cat /court/state/qemu-debug3.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug3 done"
