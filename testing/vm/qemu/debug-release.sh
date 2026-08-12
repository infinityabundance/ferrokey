#!/usr/bin/env bash
# One-off debug: daemon release_all after client SIGKILL, verified at the
# guest kernel level with evtest.
set -e
TAG=rel-$$
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
packages: [evtest, xserver-xorg-video-dummy, xserver-xorg-core, x11-utils, jq, procps, python3]
runcmd:
  - [ bash, -c, "modprobe uinput; echo uinput > /etc/modules-load.d/ferrokey-uinput.conf" ]
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
sudo env RUST_LOG=info ./payload/bin/ferrokeyd --config ./payload/fixtures/ferrokeyd.yaml >/tmp/fkd.log 2>&1 &
sleep 1
# Client A: hold LEFTSHIFT (42) for 60s
python3 ./payload/courts/fk-client.py --socket /tmp/ferrokeyd.sock handshake key-down 42 --hold 60 >/tmp/clientA.log 2>&1 &
CLIENTA=$!
sleep 2
NODE=$(ls /dev/input/event* | tail -1)
echo "device node: $NODE"
sudo timeout 30 evtest --grab "$NODE" > /tmp/evtest.log 2>&1 &
EVTEST=$!
sleep 1
echo "--- SIGKILL client A while holding LEFTSHIFT ---"
kill -9 $CLIENTA
sleep 2
echo "--- evtest capture ---"
cat /tmp/evtest.log
echo "--- daemon log ---"
cat /tmp/fkd.log
kill $EVTEST 2>/dev/null || true
exit 0
SEOF
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $KEY -P $SSHPORT /tmp/$TAG-inner.sh court@127.0.0.1:/tmp/inner.sh
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $KEY -p $SSHPORT court@127.0.0.1 'timeout 60 bash /tmp/inner.sh' 2>&1 || true

qemu_pid=$(cat /court/state/qemu-$TAG.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug done ($TAG)"
