#!/usr/bin/env bash
# One-off debug: does KWin honour keyboard_interactivity=none on a layer
# surface? Boots a wayland-profile VM, starts kwin + the wayland target + the
# layer probe, clicks the layer surface, and reports who has keyboard focus.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debuglp-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
qemu-img resize "$OVERLAY" 6G
rm -f $STATE/keys/debuglp*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debuglp
PUB=$(cat $STATE/keys/debuglp.pub)

cp /repo/testing/vm/cloud-init/user-data.wayland.yaml $STATE/seeds/debuglp-userdata.yaml
sed -i "s|__COURT_SSH_PUBKEY__|$PUB|" $STATE/seeds/debuglp-userdata.yaml
echo "instance-id: debuglp-$(date +%s%N)" > $STATE/seeds/debuglp-meta.yaml
cloud-localds $STATE/seeds/debuglp-seed.iso $STATE/seeds/debuglp-userdata.yaml $STATE/seeds/debuglp-meta.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debuglp-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22989-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debuglp.log" -monitor none -daemonize

for i in $(seq 1 180); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debuglp -p 22989 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debuglp -p 22989 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debuglp -p 22989 court@127.0.0.1 'mkdir -p payload/bin payload/courts'
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debuglp -P 22989 \
  $STATE/payload/bin/ferrokey-test-target-wayland \
  $STATE/payload/bin/ferrokey-test-layer-probe \
  court@127.0.0.1:payload/bin/ 2>/dev/null || true
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debuglp -P 22989 \
  $STATE/payload/courts/recv-events.py court@127.0.0.1:payload/courts/ 2>/dev/null || true
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debuglp -p 22989 court@127.0.0.1 'chmod +x payload/bin/*'

ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debuglp -p 22989 court@127.0.0.1 '
  export XDG_RUNTIME_DIR=/run/user/1000
  sudo mkdir -p /run/user/1000 && sudo chown court:court /run/user/1000 && sudo chmod 700 /run/user/1000
  export DISPLAY=:0
  Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 &
  sleep 3
  openbox >/tmp/openbox.log 2>&1 &
  sleep 2
  export KWIN_COMPOSE=Q
  dbus-run-session -- kwin_wayland --socket wayland-court-0 >/tmp/kwin.log 2>&1 &
  sleep 8
  env WAYLAND_DISPLAY=wayland-court-0 XDG_RUNTIME_DIR=/run/user/1000 TARGET_SOCKET=/tmp/ferrokey-test-target.sock \
      ./payload/bin/ferrokey-test-target-wayland >/tmp/target.log 2>&1 &
  sleep 2
  python3 payload/courts/recv-events.py /tmp/ferrokey-test-target.sock > /tmp/events.log 2>/dev/null &
  sleep 1
  # Layer probe: keyboard_interactivity=none layer surface.
  env WAYLAND_DISPLAY=wayland-court-0 XDG_RUNTIME_DIR=/run/user/1000 \
      timeout 40 ./payload/bin/ferrokey-test-layer-probe > /tmp/probe.log 2>&1 &
  PROBE_PID=$!
  sleep 4
  echo "== probe startup:"
  head -8 /tmp/probe.log
  echo "== probe alive? $(kill -0 $PROBE_PID 2>/dev/null && echo yes || echo no)"
  echo "== kwin log:"
  tail -5 /tmp/kwin.log
  # Focus the target.
  xdotool mousemove 300 150 click 1
  sleep 1
  echo "== target events after focus click:"
  cat /tmp/events.log
  # Click the probe layer surface (bottom area, like the OSK).
  WID=$(xwininfo -root -tree | grep -oE "0x[0-9a-f]+ \"KDE Wayland Compositor" | awk "{print \$1}" | head -1)
  GEO=$(xdotool getwindowgeometry --shell $WID)
  eval "$GEO"
  CX=$((X + 134)); CY=$((Y + HEIGHT - 342 + 228))
  echo "clicking layer surface at $CX,$CY"
  xdotool mousemove $CX $CY click 1
  sleep 1.5
  echo "== target events after layer click:"
  cat /tmp/events.log
  echo "== probe events:"
  cat /tmp/probe.log
' || true

qemu_pid=$(cat /court/state/qemu-debuglp.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debuglp done"
