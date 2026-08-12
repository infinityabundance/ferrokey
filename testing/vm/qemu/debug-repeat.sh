#!/usr/bin/env bash
# One-off debug: daemon repeat key-downs vs evtest capture.
set -e
TAG=rep-$$
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
packages: [evtest, jq, procps, python3]
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
# Client A: hold open, wait for evtest, then fire repeated KEY_A downs.
python3 - > /tmp/clientA.log 2>&1 <<'PYEOF' &
import socket, struct, time
s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("/tmp/ferrokeyd.sock")
def fr(op, payload=b""):
    body = bytes([op]) + payload
    return b"FK01" + len(body).to_bytes(2, "little") + body
s.sendall(fr(1, b"\x01\x05\x00court"))
s.sendall(fr(2))
s.recv(4096)
time.sleep(4)
for i in range(11):
    s.sendall(fr(0x10, (30).to_bytes(2, "little")))
    time.sleep(0.05)
time.sleep(10)
PYEOF
CLIENTA=$!
sleep 2
NODE=$(ls /dev/input/event* | tail -1)
echo "device node: $NODE"
# Line-buffered evtest so nothing is lost on kill.
sudo stdbuf -oL timeout 15 evtest --grab "$NODE" > /tmp/evtest.log 2>&1 &
EVTEST=$!
sleep 3
echo "--- clientA.log ---"
cat /tmp/clientA.log || echo "(empty)"
echo "--- fkd.log ---"
cat /tmp/fkd.log || echo "(empty)"
echo "--- evtest events ---"
grep "value 1" /tmp/evtest.log || echo "NONE"
echo "--- daemon ---"
grep -c "key_down 30" /tmp/fkd.log || true
kill $EVTEST 2>/dev/null || true
kill %1 2>/dev/null || true
exit 0
SEOF
scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $KEY -P $SSHPORT /tmp/$TAG-inner.sh court@127.0.0.1:/tmp/inner.sh
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $KEY -p $SSHPORT court@127.0.0.1 'timeout 60 bash /tmp/inner.sh' 2>&1 || true

qemu_pid=$(cat /court/state/qemu-$TAG.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug done ($TAG)"
