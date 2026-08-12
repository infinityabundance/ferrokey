#!/usr/bin/env bash
# One-off debug: run the ferrokey UI binary in the guest exactly as the court
# does, capturing all output.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debug5-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugkey5*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugkey5
PUB=$(cat $STATE/keys/debugkey5.pub)

cat > $STATE/seeds/debug5-userdata.yaml <<EOF
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
packages: [xserver-xorg-video-dummy, xserver-xorg-core, x11-utils, xdotool, x11-xserver-utils, evtest, strace, jq, procps, python3, libgtk-3-0, openbox]
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
cloud-localds $STATE/seeds/debug5-seed.iso $STATE/seeds/debug5-userdata.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debug5-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22995-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debug5.log" -monitor none -daemonize

for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugkey5 -p 22995 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey5 -p 22995 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey5 -p 22995 court@127.0.0.1 'mkdir -p payload'
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey5 -P 22995 -r $STATE/payload/bin court@127.0.0.1:payload/ 2>/dev/null || true
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey5 -P 22995 -r $STATE/payload/fixtures court@127.0.0.1:payload/ 2>/dev/null || true
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey5 -p 22995 court@127.0.0.1 'chmod +x payload/bin/*'

echo "=== start Xorg ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey5 -p 22995 court@127.0.0.1 'Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 & sleep 3; DISPLAY=:0 xdpyinfo >/dev/null 2>&1 && echo "X OK" || tail -5 /tmp/xorg.log' || true

echo "=== ldd ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey5 -p 22995 court@127.0.0.1 'ldd payload/bin/ferrokey 2>&1 | grep -i "not found" || echo "all libs found"' || true

echo "=== run ferrokey ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey5 -p 22995 court@127.0.0.1 'timeout 6 env DISPLAY=:0 WAYLAND_DISPLAY= XDG_RUNTIME_DIR=/tmp/court-runtime RUST_LOG=trace ./payload/bin/ferrokey --config ./payload/fixtures/ferrokey.yaml >/tmp/fk.log 2>&1; echo "exit=$?"; echo "--- fk.log ---"; cat /tmp/fk.log' || true

qemu_pid=$(cat /court/state/qemu-debug5.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug5 done"
