#!/usr/bin/env bash
# One-off debug: can Weston run nested in the dummy X server (pixman
# renderer) and advertise zwlr_layer_shell_v1? Also: does the Ferrokey UI
# connect and select the layer-shell backend?
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debugwf-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugwf*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugwf
PUB=$(cat $STATE/keys/debugwf.pub)

cat > $STATE/seeds/debugwf-userdata.yaml <<EOF
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
packages: [xserver-xorg-video-dummy, xserver-xorg-core, xinit, x11-utils, xdotool, x11-xserver-utils, jq, procps, python3, openbox, weston, xwayland, wayland-utils, dbus, libgl1-mesa-dri, libxkbcommon0, libsdl2-2.0-0, libgtk-3-0]
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
cloud-localds $STATE/seeds/debugwf-seed.iso $STATE/seeds/debugwf-userdata.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debugwf-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22994-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debugwf.log" -monitor none -daemonize

for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugwf -p 22994 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwf -p 22994 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwf -p 22994 court@127.0.0.1 'mkdir -p payload/bin payload/courts'
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwf -P 22994 \
  $STATE/payload/bin/ferrokey $STATE/payload/bin/ferrokeyd \
  court@127.0.0.1:payload/bin/ 2>/dev/null || true
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwf -P 22994 \
  $STATE/payload/fixtures/ferrokey.yaml $STATE/payload/fixtures/ferrokeyd.yaml \
  court@127.0.0.1:payload/ 2>/dev/null || true

echo "=== start Xorg ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwf -p 22994 court@127.0.0.1 \
  'Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 & sleep 3; DISPLAY=:0 xdpyinfo >/dev/null 2>&1 && echo "X OK"' || true

# Fresh overlay boots from the cached debian-12 base; the backports weston is
# installed here so the test always uses the layer-shell-capable version.
echo "=== install weston from bookworm-backports ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwf -p 22994 court@127.0.0.1 \
  'sudo sh -c "echo deb http://deb.debian.org/debian bookworm-backports main > /etc/apt/sources.list.d/backports.list" && sudo apt-get update -qq 2>&1 | tail -1; sudo apt-get install -y -t bookworm-backports weston wayland-utils 2>&1 | tail -3; weston --version' || true

echo "=== start weston (x11 backend, pixman) ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwf -p 22994 court@127.0.0.1 '
  export XDG_RUNTIME_DIR=/run/user/1000
  sudo mkdir -p /run/user/1000 && sudo chown court:court /run/user/1000 && sudo chmod 700 /run/user/1000
  export DISPLAY=:0
  dbus-run-session -- weston --backend=x11-backend.so --socket=wayland-court-0 --width=1280 --height=720 \
    >/tmp/weston.log 2>&1 &
  sleep 6
  ls -la /run/user/1000/wayland-court-0 2>&1
  echo "--- weston.log ---"; head -30 /tmp/weston.log
  echo "--- globals ---"; wayland-info wayland-court-0 2>&1 | grep -iE "layer_shell|xwayland|wl_seat|wl_output" | head -10
' || true

echo "=== ferrokey UI on wayland-court-0 ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwf -p 22994 court@127.0.0.1 '
  export XDG_RUNTIME_DIR=/run/user/1000
  sudo ./payload/bin/ferrokeyd --config payload/ferrokeyd.yaml >/tmp/ferrokeyd.log 2>&1 &
  sleep 1
  env WAYLAND_DISPLAY=wayland-court-0 XDG_RUNTIME_DIR=/run/user/1000 DISPLAY= \
      ./payload/bin/ferrokey --config payload/ferrokey.yaml >/tmp/ferrokey.log 2>&1 &
  FKPID=$!
  sleep 6
  kill -0 $FKPID 2>/dev/null && echo "UI ALIVE" || echo "UI DEAD"
  echo "--- ferrokey.log ---"; cat /tmp/ferrokey.log
  echo "--- ferrokeyd.log ---"; tail -5 /tmp/ferrokeyd.log
  kill $FKPID 2>/dev/null || true
' || true

qemu_pid=$(cat /court/state/qemu-debugwf.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debugwf done"
