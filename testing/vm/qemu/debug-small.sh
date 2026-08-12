#!/usr/bin/env bash
# One-off debug: run ferrokey with a SMALL window (fits in classic 256KB
# requests) to isolate PutImage size vs pixel-format issues.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debug6-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugkey6*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugkey6
PUB=$(cat $STATE/keys/debugkey6.pub)

cat > $STATE/seeds/debug6-userdata.yaml <<EOF
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
packages: [xserver-xorg-video-dummy, xserver-xorg-core, x11-utils, xdotool, x11-xserver-utils, evtest, jq, procps, python3]
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
cloud-localds $STATE/seeds/debug6-seed.iso $STATE/seeds/debug6-userdata.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debug6-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22994-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debug6.log" -monitor none -daemonize

for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugkey6 -p 22994 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey6 -p 22994 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey6 -p 22994 court@127.0.0.1 'mkdir -p payload'
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey6 -P 22994 -r $STATE/payload/bin court@127.0.0.1:payload/ 2>/dev/null || true
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey6 -P 22994 -r $STATE/payload/fixtures court@127.0.0.1:payload/ 2>/dev/null || true
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey6 -p 22994 court@127.0.0.1 'chmod +x payload/bin/*'

echo "=== display info ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey6 -p 22994 court@127.0.0.1 'Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 & sleep 3; DISPLAY=:0 xdpyinfo | grep -E "depth of root|number of planes|dimensions" ' || true

echo "=== small window (200x100 = 80KB @ 4bpp) ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey6 -p 22994 court@127.0.0.1 'cat > /tmp/small.yaml <<EOF
layout: us
socket_path: /tmp/ferrokeyd.sock
width: 200
height: 100
x11_display: ":0"
sticky: {latch_enabled: true, lock_enabled: true, tap_timeout_ms: 400, double_tap_timeout_ms: 500}
repeat: {enabled: true, delay_ms: 500, cadence_ms: 30}
force_degraded_banner: false
EOF
timeout 5 env DISPLAY=:0 WAYLAND_DISPLAY= XDG_RUNTIME_DIR=/tmp/court-runtime RUST_LOG=trace ./payload/bin/ferrokey --config /tmp/small.yaml >/tmp/fk-small.log 2>&1; echo "exit=$?"; tail -5 /tmp/fk-small.log' || true

qemu_pid=$(cat /court/state/qemu-debug6.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug6 done"
