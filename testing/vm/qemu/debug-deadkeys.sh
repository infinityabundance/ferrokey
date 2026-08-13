#!/usr/bin/env bash
# One-off debug: is RightAlt+E → é under `setxkbmap us -variant intl` in this
# guest? Probes three paths and reports keyvals:
#   A. XTEST via xdotool (core keymap)
#   B. XTEST with explicit ISO_Level3_Shift
#   C. a real uinput device via ferrokey-test-virtinput (the Ferrokey path)
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debugdk-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugdk*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugdk
PUB=$(cat $STATE/keys/debugdk.pub)

cat > $STATE/seeds/debugdk-userdata.yaml <<EOF
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
packages: [xserver-xorg-video-dummy, xserver-xorg-core, xinit, x11-utils, xdotool, x11-xserver-utils, evtest, strace, jq, procps, python3, openbox, libgtk-3-0]
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
cloud-localds $STATE/seeds/debugdk-seed.iso $STATE/seeds/debugdk-userdata.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debugdk-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22995-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debugdk.log" -monitor none -daemonize

for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugdk -p 22995 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -p 22995 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -p 22995 court@127.0.0.1 'mkdir -p payload/bin payload/courts'
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -P 22995 \
  $STATE/payload/bin/ferrokey-test-target-gtk \
  $STATE/payload/bin/ferrokey-test-virtinput \
  court@127.0.0.1:payload/bin/ 2>/dev/null || true
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -P 22995 \
  $STATE/payload/courts/recv-events.py court@127.0.0.1:payload/courts/ 2>/dev/null || true
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -p 22995 court@127.0.0.1 'chmod +x payload/bin/*'

echo "=== start Xorg + openbox ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -p 22995 court@127.0.0.1 'Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 & sleep 3; DISPLAY=:0 openbox >/tmp/openbox.log 2>&1 & sleep 2; DISPLAY=:0 xdpyinfo >/dev/null 2>&1 && echo "X OK"' || true

echo "=== keymap after setxkbmap us -variant intl ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -p 22995 court@127.0.0.1 \
  'export DISPLAY=:0; setxkbmap us -variant intl; setxkbmap -query; echo "--- keycode 100/105: ---"; xmodmap -pke | grep -E "keycode  (100|105)"' || true

echo "=== probe A: XTEST Alt_R + e (xdotool) ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -p 22995 court@127.0.0.1 '
  export DISPLAY=:0
  timeout 6 ./payload/bin/ferrokey-test-target-gtk >/tmp/target.log 2>&1 &
  sleep 1
  python3 payload/courts/recv-events.py /tmp/ferrokey-test-target.sock > /tmp/eventsA.log 2>/dev/null &
  sleep 1
  xdotool mousemove 640 400 click 1
  sleep 0.5
  xdotool keydown Alt_R; sleep 0.2; xdotool key e; sleep 0.2; xdotool keyup Alt_R
  sleep 1
  echo "--- events A ---"; cat /tmp/eventsA.log
  pkill -f ferrokey-test-target-gtk; pkill -f recv-events.py; sleep 0.5
' || true

echo "=== probe B: XTEST ISO_Level3_Shift + e (xdotool) ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -p 22995 court@127.0.0.1 '
  export DISPLAY=:0
  timeout 6 ./payload/bin/ferrokey-test-target-gtk >/tmp/target.log 2>&1 &
  sleep 1
  python3 payload/courts/recv-events.py /tmp/ferrokey-test-target.sock > /tmp/eventsB.log 2>/dev/null &
  sleep 1
  xdotool mousemove 640 400 click 1
  sleep 0.5
  xdotool keydown ISO_Level3_Shift; sleep 0.2; xdotool key e; sleep 0.2; xdotool keyup ISO_Level3_Shift
  sleep 1
  echo "--- events B ---"; cat /tmp/eventsB.log
  pkill -f ferrokey-test-target-gtk; pkill -f recv-events.py; sleep 0.5
' || true

echo "=== probe C: uinput device (virtinput) RightAlt(100) + E(18) ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -p 22995 court@127.0.0.1 '
  export DISPLAY=:0
  timeout 6 ./payload/bin/ferrokey-test-target-gtk >/tmp/target.log 2>&1 &
  sleep 1
  python3 payload/courts/recv-events.py /tmp/ferrokey-test-target.sock > /tmp/eventsC.log 2>/dev/null &
  sleep 1
  xdotool mousemove 640 400 click 1
  sleep 0.5
  printf "key-down 100\nkey-down 18\nsleep 100\nkey-up 18\nkey-up 100\n" | sudo ./payload/bin/ferrokey-test-virtinput >/tmp/virt.log 2>&1
  sleep 1
  echo "--- events C ---"; cat /tmp/eventsC.log
  echo "--- virtinput log ---"; cat /tmp/virt.log
  pkill -f ferrokey-test-target-gtk; pkill -f recv-events.py; sleep 0.5
' || true

echo "=== probe D: uinput device ISO_Level3_Shift(92) + E(18) ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugdk -p 22995 court@127.0.0.1 '
  export DISPLAY=:0
  timeout 6 ./payload/bin/ferrokey-test-target-gtk >/tmp/target.log 2>&1 &
  sleep 1
  python3 payload/courts/recv-events.py /tmp/ferrokey-test-target.sock > /tmp/eventsD.log 2>/dev/null &
  sleep 1
  xdotool mousemove 640 400 click 1
  sleep 0.5
  printf "key-down 92\nkey-down 18\nsleep 100\nkey-up 18\nkey-up 92\n" | sudo ./payload/bin/ferrokey-test-virtinput >/tmp/virt.log 2>&1
  sleep 1
  echo "--- events D ---"; cat /tmp/eventsD.log
  pkill -f ferrokey-test-target-gtk; pkill -f recv-events.py; sleep 0.5
' || true

qemu_pid=$(cat /court/state/qemu-debugdk.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debugdk done"
