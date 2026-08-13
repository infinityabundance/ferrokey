#!/usr/bin/env bash
# One-off debug: boot the RETAINED wayland failed overlay and probe KWin's
# focus behavior when a keyboard_interactivity=none layer surface is clicked.
set -e
STATE=/court/state
FAILED=$STATE/evidence/wayland/failed-overlay.qcow2
OVERLAY=$STATE/overlays/debugwf2-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$FAILED" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugwf2*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugwf2
PUB=$(cat $STATE/keys/debugwf2.pub)

cat > $STATE/seeds/debugwf2-userdata.yaml <<EOF
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
echo "instance-id: debugwf2-$(date +%s%N)" > $STATE/seeds/debugwf2-meta.yaml
cloud-localds $STATE/seeds/debugwf2-seed.iso $STATE/seeds/debugwf2-userdata.yaml $STATE/seeds/debugwf2-meta.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debugwf2-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22990-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debugwf2.log" -monitor none -daemonize

for i in $(seq 1 120); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugwf2 -p 22990 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwf2 -p 22990 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'

ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwf2 -p 22990 court@127.0.0.1 '
  export XDG_RUNTIME_DIR=/run/user/1000
  export DISPLAY=:0
  # Recreate the court flow: Xorg + kwin + target + recorder.
  Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 &
  sleep 3
  openbox >/tmp/openbox.log 2>&1 &
  sleep 2
  dbus-run-session -- kwin_wayland --socket wayland-court-0 >/tmp/kwin.log 2>&1 &
  sleep 8
  env WAYLAND_DISPLAY=wayland-court-0 XDG_RUNTIME_DIR=/run/user/1000 TARGET_SOCKET=/tmp/ferrokey-test-target.sock \
      ./payload/bin/ferrokey-test-target-wayland >/tmp/target.log 2>&1 &
  sleep 2
  python3 payload/courts/recv-events.py /tmp/ferrokey-test-target.sock > /tmp/events.log 2>/dev/null &
  sleep 1
  # Focus the target: click at (300,150).
  xdotool mousemove 300 150 click 1
  sleep 1
  echo "== after focus click:"
  cat /tmp/events.log
  echo "== x input focus on :0:"
  xdotool getwindowfocus
  xwininfo -id $(xdotool getwindowfocus) 2>/dev/null | grep -E "Window id|xwininfo" | head -2
  # Click the OSK region (bottom area) WITHOUT ferrokey running: does the
  # TARGET lose keyboard focus when the pointer clicks on empty bottom space?
  xdotool mousemove 400 600 click 1
  sleep 1
  echo "== after bottom click:"
  cat /tmp/events.log
' || true

qemu_pid=$(cat /court/state/qemu-debugwf2.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debugwf2 done"
