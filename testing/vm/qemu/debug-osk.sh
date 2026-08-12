#!/usr/bin/env bash
# One-off debug: full X11 stack — OSK geometry, WM_HINTS, click injection.
set -e
TAG=osk-$$
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/$TAG-$(date +%s%N).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
KEY=$STATE/keys/$TAG
rm -f $KEY*
ssh-keygen -q -t ed25519 -N "" -f $KEY
PUB=$(cat $KEY.pub)

USERDATA=$STATE/seeds/$TAG-userdata.yaml
cat > $USERDATA <<EOF
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
packages: [xserver-xorg-video-dummy, xserver-xorg-core, x11-utils, xdotool, x11-xserver-utils, openbox, evtest, jq, procps, python3]
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
SEED=$STATE/seeds/$TAG-seed.iso
cloud-localds $SEED $USERDATA

SSHPORT=$(( 22900 + (RANDOM % 90) ))
qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$SEED,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:$SSHPORT-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-$TAG.log" -monitor none -daemonize

echo "debug VM on port $SSHPORT"
for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $KEY -p $SSHPORT court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $KEY -p $SSHPORT court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $KEY -p $SSHPORT court@127.0.0.1 'mkdir -p payload'
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $KEY -P $SSHPORT -r $STATE/payload/. court@127.0.0.1:payload/ 2>/dev/null || true
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $KEY -p $SSHPORT court@127.0.0.1 'chmod +x payload/bin/*'

cat > /tmp/$TAG-inner.sh <<'SEOF'
#!/usr/bin/env bash
set -x
Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 &
sleep 3
DISPLAY=:0 openbox >/tmp/openbox.log 2>&1 &
sleep 2
python3 /home/court/payload/courts/recv-events.py /tmp/ferrokey-test-target.sock > /tmp/events.log 2>&1 &
sleep 1
DISPLAY=:0 env TARGET_SOCKET=/tmp/ferrokey-test-target.sock ./payload/bin/ferrokey-test-target-x11 >/tmp/target.log 2>&1 &
sleep 2
sudo env RUST_LOG=info ./payload/bin/ferrokeyd --config ./payload/fixtures/ferrokeyd.yaml >/tmp/ferrokeyd.log 2>&1 &
sleep 1
DISPLAY=:0 WAYLAND_DISPLAY= XDG_RUNTIME_DIR=/tmp/court-runtime ./payload/bin/ferrokey --config ./payload/fixtures/ferrokey.yaml >/tmp/ferrokey.log 2>&1 &
sleep 4
echo "=== windows ==="
echo "--- xdotool search Ferrokey ---"
DISPLAY=:0 xdotool search --name "Ferrokey" 2>&1 || echo "no match"
echo "--- xdotool search full title ---"
DISPLAY=:0 xdotool search --name "Ferrokey Virtual Keyboard" 2>&1 || echo "no match"
echo "--- xdotool search --class ---"
DISPLAY=:0 xdotool search --class "ferrokey" 2>&1 || echo "no class match"
echo "--- root tree ---"
DISPLAY=:0 xwininfo -root -tree 2>&1 | grep -iE "ferrokey|0x" | head -12
echo "--- target.log ---"
cat /tmp/target.log
echo "=== WM_HINTS ==="
DISPLAY=:0 xprop -name "Ferrokey Virtual Keyboard" WM_HINTS 2>&1 | head -8
echo "=== events before ==="
cat /tmp/events.log
echo "=== focus target ==="
W=$(DISPLAY=:0 xdotool search --name ferrokey-test-target | head -1)
DISPLAY=:0 xdotool windowactivate --sync $W 2>&1
sleep 1
cat /tmp/events.log
echo "=== click OSK a (134,206) ==="
DISPLAY=:0 xdotool mousemove 134 206 click 1
sleep 1
echo "=== events after click ==="
cat /tmp/events.log
echo "=== focus now ==="
DISPLAY=:0 xdotool getwindowfocus
echo "=== ferrokeyd ==="
cat /tmp/ferrokeyd.log
exit 0
SEOF
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $KEY -P $SSHPORT /tmp/$TAG-inner.sh court@127.0.0.1:/tmp/inner.sh
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $KEY -p $SSHPORT court@127.0.0.1 'timeout 60 bash /tmp/inner.sh' 2>&1 || true

qemu_pid=$(cat /court/state/qemu-$TAG.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug done ($TAG)"
