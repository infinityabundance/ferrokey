#!/usr/bin/env bash
# One-off debug: kwin_wayland nested in the dummy X server (QPainter software
# compositing) with layer-shell.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debugkw-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
qemu-img resize "$OVERLAY" 10G
rm -f $STATE/keys/debugkw*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugkw
PUB=$(cat $STATE/keys/debugkw.pub)

cat > $STATE/seeds/debugkw-userdata.yaml <<EOF
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
packages: [xserver-xorg-video-dummy, xserver-xorg-core, xinit, x11-utils, xdotool, x11-xserver-utils, jq, procps, python3, openbox, xwayland, wayland-utils, dbus, kwin-wayland, libxkbcommon0, fonts-dejavu-core]
runcmd:
  - sh -c 'echo "deb http://deb.debian.org/debian bookworm-backports main" > /etc/apt/sources.list.d/backports.list'
  - apt-get update
  - DEBIAN_FRONTEND=noninteractive apt-get install -y -t bookworm-backports kwin-wayland kwin-common kwin-data
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
cloud-localds $STATE/seeds/debugkw-seed.iso $STATE/seeds/debugkw-userdata.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debugkw-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22992-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debugkw.log" -monitor none -daemonize

for i in $(seq 1 180); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugkw -p 22992 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkw -p 22992 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkw -p 22992 court@127.0.0.1 'mkdir -p payload/bin payload/courts'
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkw -P 22992 \
  $STATE/payload/bin/ferrokey $STATE/payload/bin/ferrokeyd \
  court@127.0.0.1:payload/bin/ 2>/dev/null || true
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkw -P 22992 \
  $STATE/payload/fixtures/ferrokey.yaml $STATE/payload/fixtures/ferrokeyd.yaml \
  court@127.0.0.1:payload/ 2>/dev/null || true

echo "=== start Xorg ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkw -p 22992 court@127.0.0.1 \
  'Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 & sleep 3; DISPLAY=:0 xdpyinfo >/dev/null 2>&1 && echo "X OK"' || true

echo "=== start kwin_wayland (x11 backend, QPainter) ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkw -p 22992 court@127.0.0.1 '
  export XDG_RUNTIME_DIR=/run/user/1000
  sudo mkdir -p /run/user/1000 && sudo chown court:court /run/user/1000 && sudo chmod 700 /run/user/1000
  export DISPLAY=:0
  export KWIN_COMPOSE=Q
  kwin_wayland --version 2>&1 | head -1
  # No backend flag: KWin auto-selects the x11 backend when DISPLAY is set.
  dbus-run-session -- kwin_wayland --socket wayland-court-0 >/tmp/kwin.log 2>&1 &
  sleep 8
  ls -la /run/user/1000/wayland-court-0 2>&1
  echo "--- kwin.log ---"; head -20 /tmp/kwin.log
  echo "--- globals ---"; wayland-info wayland-court-0 2>&1 | grep -iE "layer_shell|xwayland|wl_seat|wl_output" | head
' || true

echo "=== ferrokey UI on wayland-court-0 ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkw -p 22992 court@127.0.0.1 '
  export XDG_RUNTIME_DIR=/run/user/1000
  sudo ./payload/bin/ferrokeyd --config payload/ferrokeyd.yaml >/tmp/ferrokeyd.log 2>&1 &
  sleep 1
  env WAYLAND_DISPLAY=wayland-court-0 XDG_RUNTIME_DIR=/run/user/1000 DISPLAY= \
      ./payload/bin/ferrokey --config payload/ferrokey.yaml >/tmp/ferrokey.log 2>&1 &
  FKPID=$!
  sleep 6
  kill -0 $FKPID 2>/dev/null && echo "UI ALIVE" || echo "UI DEAD"
  echo "--- ferrokey.log ---"; cat /tmp/ferrokey.log
  echo "--- kwin.log tail ---"; tail -5 /tmp/kwin.log
  kill $FKPID 2>/dev/null || true
' || true

qemu_pid=$(cat /court/state/qemu-debugkw.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debugkw done"
