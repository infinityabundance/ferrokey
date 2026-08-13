#!/usr/bin/env bash
# One-off debug: boot the RETAINED wayland-court failed overlay and probe the
# OSK click path: kwin geometry at click time + what the UI does with clicks.
set -e
STATE=/court/state
FAILED=$STATE/evidence/wayland/failed-overlay.qcow2
OVERLAY=$STATE/overlays/debugwl-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$FAILED" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugwl*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugwl
PUB=$(cat $STATE/keys/debugwl.pub)

cat > $STATE/seeds/debugwl-userdata.yaml <<EOF
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
echo "instance-id: debugwl-$(date +%s%N)" > $STATE/seeds/debugwl-meta.yaml
cloud-localds $STATE/seeds/debugwl-seed.iso $STATE/seeds/debugwl-userdata.yaml $STATE/seeds/debugwl-meta.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debugwl-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22991-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debugwl.log" -monitor none -daemonize

for i in $(seq 1 120); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugwl -p 22991 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwl -p 22991 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'

# Reuse the court's own payload + courts (already on the overlay from the court run).
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugwl -p 22991 court@127.0.0.1 '
  export DISPLAY=:0
  echo "== X windows:"
  xwininfo -root -tree | grep -E "KDE Wayland|Openbox" | head -4
  echo "== kwin window geometry:"
  WID=$(xwininfo -root -tree | grep -oE "0x[0-9a-f]+ \"KDE Wayland Compositor" | awk "{print \$1}" | head -1)
  echo "WID=$WID"
  xdotool getwindowgeometry --shell $WID
  echo "== compute a-key click:"
  POS=$(python3 payload/courts/osk-geometry.py a)
  KX=${POS%,*}; KY=${POS#*,}
  echo "KX=$KX KY=$KY"
  GEO=$(xdotool getwindowgeometry --shell $WID)
  eval "$GEO"
  CX=$((X + KX)); CY=$((Y + HEIGHT - 342 + KY))
  echo "click at $CX,$CY"
  echo "== inject the click:"
  xdotool mousemove $CX $CY click 1
  sleep 1
  echo "== ferrokeyd log after click:"
  tail -6 court-output/ferrokeyd.log
  echo "== events.log:"
  tail -4 court-output/events.log
' || true

qemu_pid=$(cat /court/state/qemu-debugwl.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debugwl done"
