#!/usr/bin/env bash
# One-off debug: raw X11 PutImage probe — finds what byte layout/size the
# guest Xorg accepts for a depth-24 window.
set -e
STATE=/court/state
BASE=$STATE/images/debian-12.qcow2
OVERLAY=$STATE/overlays/debug7-$(date +%s).qcow2
qemu-img create -q -f qcow2 -b "$BASE" -F qcow2 "$OVERLAY"
rm -f $STATE/keys/debugkey7*
ssh-keygen -q -t ed25519 -N "" -f $STATE/keys/debugkey7
PUB=$(cat $STATE/keys/debugkey7.pub)

cat > $STATE/seeds/debug7-userdata.yaml <<EOF
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
packages: [xserver-xorg-video-dummy, xserver-xorg-core, x11-utils, jq, procps, python3]
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
cloud-localds $STATE/seeds/debug7-seed.iso $STATE/seeds/debug7-userdata.yaml

qemu-system-x86_64 -machine accel=kvm -cpu host -m 2048 -smp 2 \
  -drive "file=$OVERLAY,format=qcow2,if=virtio" \
  -drive "file=$STATE/seeds/debug7-seed.iso,format=raw,if=virtio" \
  -netdev "user,id=n1,hostfwd=tcp:127.0.0.1:22993-:22" \
  -device virtio-net-pci,netdev=n1 -display none \
  -serial "file:$STATE/logs/qemu-debug7.log" -monitor none -daemonize

for i in $(seq 1 150); do
  if ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=3 -o LogLevel=ERROR -i $STATE/keys/debugkey7 -p 22993 court@127.0.0.1 true 2>/dev/null; then break; fi
  sleep 2
done
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey7 -p 22993 court@127.0.0.1 'sudo cloud-init status --wait >/dev/null 2>&1 || true'

cat > /tmp/putimage-probe.py <<'PYEOF'
"""Raw X11 probe: create a depth-24 window, try PutImage with several
byte layouts, report which succeed (no error event)."""
import socket, struct, sys

s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
s.connect("/tmp/.X11-unix/X0")
f = s.makefile("rb", buffering=0)

def send(b): s.sendall(b)
def recvn(n):
    out = b""
    while len(out) < n:
        chunk = f.read(n - len(out))
        if not chunk: raise EOFError
        out += chunk
    return out

# Setup request: byte-order 'l', protocol 11.0, no auth
setup = b"l" + b"\x00" + struct.pack("<HH", 11, 0) + struct.pack("<HH", 0, 0) + b"\x00" * 8
send(struct.pack("<H", 2) + setup)  # hmm: setup is a 4-byte-aligned blob; first 2 bytes length?
# Actually xConnSetupPrefix: 1 byte success, 1 pad, 2 length, then xConnClientPrefix
# We send: success=1, pad, length in 4-byte units of the whole setup, then client prefix.
prefix = struct.pack("<BBH", 1, 0, 0)  # success, pad, length (patched below)
client = b"l" + b"\x00" + struct.pack("<HH", 11, 0) + struct.pack("<HH", 0, 0) + b"\x00" * 8
body = prefix + client
body = body[:2] + struct.pack("<H", len(body) // 4) + body[4:]
send(body)

def parse_setup():
    hdr = recvn(8)
    success = hdr[0]
    length = struct.unpack("<H", hdr[2:4])[0]
    rest = recvn(length * 4 - 8)
    return success, hdr + rest

success, data = parse_setup()
assert success == 1, f"setup failed: {data[:8]}"

# Parse setup for root depth, root window id, screen info
# xConnSetup: release(4) ridbase(4) ridmask(4) motionbuf(4) nbytesvendor(2)
# maxrequest(2) numroots(1) numformats(1) imagebyteorder(1) bitmapbitorder(1)
# bitmapscanlineunit(1) bitmapscanlinepad(1) minkeycode(1) maxkeycode(1)
# pad(4) vendor... roots...
off = 8
release, ridbase, ridmask, motion = struct.unpack("<IIII", data[off:off+16]); off += 16
nbytesvendor, maxrequest = struct.unpack("<HH", data[off:off+4]); off += 4
numroots, numformats = data[off], data[off+1]; off += 2
off += 6  # byteorders, scanline, keycodes, pad
off += (nbytesvendor + 3) & ~3
# formats
formats = []
for i in range(numformats):
    depth, bpp, pad2 = struct.unpack("<BBH", data[off:off+4]); off += 4
    formats.append((depth, bpp, pad2))
    off += 4
# roots
rootinfo = struct.unpack("<IIIIHHIII", data[off:off+32]); off += 32
root_wid, cmap, wpix, bpix, root_x, root_y, root_w, root_h, root_depth = rootinfo
print(f"root depth={root_depth} size={root_w}x{root_h} formats={formats}", flush=True)
# depth list: nDepths(1) pad(1) then xDepth entries
ndepths = data[off]; off += 2
for d in range(ndepths):
    depth, nvisuals = struct.unpack("<BH", data[off:off+3]); off += 3
    off += 5
    off += nvisuals * 24
n = 0
seq = 1
def req(op, body):
    global seq
    pkt = struct.pack("<BBH", op, 0, 0) + body
    pkt = pkt[:2] + struct.pack("<H", len(pkt) // 4) + pkt[4:]
    send(pkt)
    seq += 1
    return seq - 1

# CreateWindow (depth 24, class 0 inputoutput, no mask)
wid = ridbase | 1
gc = ridbase | 2
req(1, struct.pack("<IIHHHhHBBIII", wid, root_wid, 200, 100, 0, 0, 0, 24, 0, 0, 0, 0))
# CreateGC (no mask)
req(55, struct.pack("<II", gc, wid) + struct.pack("<I", 0))
# MapWindow
req(8, struct.pack("<I", wid))

def putimage(datasize, depth=24, w=200, h=100):
    """Send PutImage with exactly `datasize` bytes of data; return (error_code, error_value) or None."""
    global seq
    hdr = struct.pack("<BBHIIHHhhBB", 72, 2, 0, wid, gc, w, h, 0, 0, 0, depth)
    pkt = hdr + b"\x00" * 2 + b"\x00" * datasize
    total = len(pkt)
    pkt = pkt[:2] + struct.pack("<H", total // 4 if total // 4 < 65536 else 0) + pkt[4:]
    if total // 4 >= 65536:
        # big request: length 0 + 32-bit length
        big = struct.pack("<I", total // 4 + 1)
        pkt = pkt[:4] + big + pkt[4:]
    send(pkt)
    seq += 1
    # read until we get an error or enough replies
    s.settimeout(1.0)
    try:
        while True:
            ev = recvn(32)
            if ev[0] == 0:  # error
                return (ev[1], struct.unpack("<I", ev[4:8])[0])
            # events/replies: check for reply (type 1) — but PutImage has no reply
            # 32-byte events; keep draining
    except (socket.timeout, EOFError):
        return None

# Try: 3 bytes/px packed (60000), 4 bytes/px (80000), and a few others
for label, size in [
    ("3bpp packed 200x100 = 60000", 60000),
    ("4bpp 200x100 = 80000", 80000),
    ("pad-to-64 3bpp 200x100 = 60416", 60416),
    ("2 bytes/px = 40000", 40000),
]:
    err = putimage(size)
    print(f"{label}: {'OK' if err is None else 'ERR code=' + str(err[0]) + ' val=' + hex(err[1])}", flush=True)
PYEOF

ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey7 -p 22993 court@127.0.0.1 'Xorg :0 -noreset -nolisten tcp >/tmp/xorg.log 2>&1 & sleep 3; DISPLAY=:0 xdpyinfo | grep -E "depth of root"'
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i $STATE/keys/debugkey7 -p 22993 court@127.0.0.1 'python3 -' < /tmp/putimage-probe.py || true

qemu_pid=$(cat /court/state/qemu-debug7.pid 2>/dev/null || true)
[ -n "$qemu_pid" ] && kill "$qemu_pid" 2>/dev/null || true
rm -f "$OVERLAY"
echo "debug7 done"
