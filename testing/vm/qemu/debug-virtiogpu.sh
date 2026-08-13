#!/usr/bin/env bash
# One-off debug: virtio-gpu in the guest → DRI3-capable X server (modesetting)
# → wayfire (wlroots) nested x11 backend with layer-shell.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debugvg-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugvg*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugvg
PUB=$(cat $STATE/keys/debugvg.pub)

cat > $STATE/seeds/debugvg-userdata.yaml <<EOF
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
packages: [xserver-xorg-core, xinit, x11-utils, xdotool, x11-xserver-utils, jq, procps, python3, openbox, wayfire, xwayland, wayland-utils, dbus, libgl1-mesa-dri, libxkbcommon0, xserver-xorg-video-dummy]
write_files:
  - path: /etc/X11/Xwrapper.config
    content: |
      allowed_users=anybody
      needs_root_rights=yes
  - path: /etc/X11/xorg.conf.d/99-ferrokey-virtio.conf
    content: |
      Section "Device"
          Identifier  "VirtioGPU"
          Driver      "modesetting"
          Option      "AccelMethod" "glamor"
      EndSection
      Section "Screen"
          Identifier  "VirtioScreen"
          Device      "VirtioGPU"
          DefaultDepth 24
          SubSection "Display"
              Depth 24
              Modes "1280x720"
          EndSubSection
      EndSection
EOF
cloud-localds $STATE/seeds/debugvg-seed.iso $STATE/seeds/debugvg-userdata.yaml

# virtio-vga-gl: DRM + virgl-capable display device. egl-headless renders
# the guest's GL on the host (llvmpipe inside the oracle container). DRI3
# comes from the modesetting driver + glamor over virgl.
qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -device virtio-vga-gl \
  -display egl-headless \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debugvg-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22993-:22" \
  -device virtio-net-pci,netdev=n1 -serial "file:$STATE/logs/qemu-debugvg.log" -monitor none -daemonize

for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugvg -p 22993 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugvg -p 22993 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugvg -p 22993 court@127.0.0.1 'mkdir -p payload/bin payload/courts'
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugvg -P 22993 \
  $STATE/payload/bin/ferrokey $STATE/payload/bin/ferrokeyd \
  court@127.0.0.1:payload/bin/ 2>/dev/null || true
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugvg -P 22993 \
  $STATE/payload/fixtures/ferrokey.yaml $STATE/payload/fixtures/ferrokeyd.yaml \
  court@127.0.0.1:payload/ 2>/dev/null || true

echo "=== drm devices in the guest ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugvg -p 22993 court@127.0.0.1 \
  'ls -la /dev/dri/ 2>&1; lsmod | grep -E "virtio|drm" | head -5' || true

echo "=== start Xorg (modesetting on virtio-gpu) ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugvg -p 22993 court@127.0.0.1 \
  'Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 & sleep 4; DISPLAY=:0 xdpyinfo >/dev/null 2>&1 && echo "X OK"; echo "--- DRI3 check: ---"; DISPLAY=:0 xdpyinfo | grep -i dri3 || echo "NO DRI3"; echo "--- GLX check: ---"; DISPLAY=:0 glxinfo 2>/dev/null | head -3 || echo "no glxinfo"; echo "--- xorg.log errors: ---"; grep -iE "error|fail|fatal" /tmp/xorg.log | head -5' || true

echo "=== start wayfire (x11 backend) ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugvg -p 22993 court@127.0.0.1 '
  export XDG_RUNTIME_DIR=/run/user/1000
  sudo mkdir -p /run/user/1000 && sudo chown court:court /run/user/1000 && sudo chmod 700 /run/user/1000
  export DISPLAY=:0
  dbus-run-session -- wayfire -s wayland-court-0 >/tmp/wayfire.log 2>&1 &
  sleep 6
  ls -la /run/user/1000/wayland-court-0 2>&1
  echo "--- wayfire.log ---"; head -25 /tmp/wayfire.log
  echo "--- globals ---"; wayland-info wayland-court-0 2>&1 | grep -iE "layer_shell|xwayland|wl_seat|wl_output" | head
' || true

echo "=== ferrokey UI on wayland-court-0 ==="
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugvg -p 22993 court@127.0.0.1 '
  export XDG_RUNTIME_DIR=/run/user/1000
  sudo ./payload/bin/ferrokeyd --config payload/ferrokeyd.yaml >/tmp/ferrokeyd.log 2>&1 &
  sleep 1
  env WAYLAND_DISPLAY=wayland-court-0 XDG_RUNTIME_DIR=/run/user/1000 DISPLAY= \
      ./payload/bin/ferrokey --config payload/ferrokey.yaml >/tmp/ferrokey.log 2>&1 &
  FKPID=$!
  sleep 6
  kill -0 $FKPID 2>/dev/null && echo "UI ALIVE" || echo "UI DEAD"
  echo "--- ferrokey.log ---"; cat /tmp/ferrokey.log
  kill $FKPID 2>/dev/null || true
' || true

qemu_pid=$(cat /court/state/qemu-debugvg.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debugvg done"
