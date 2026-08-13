#!/usr/bin/env bash
# Bake the cached "browsers" court base image (rule 5: immutable base images).
#
# Boots a throwaway VM over the plain Debian base, installs the X11 stack +
# Firefox + Chromium + SDL + Electron ONCE, and snapshots the disk as
# `debian-12-browsers.qcow2`. The firefox/chromium/electron courts boot
# disposable overlays over THIS image, so the ~1.5 GB browser install happens
# exactly once instead of on every court run.
#
# usage: build-browsers-image.sh <state-dir>
set -euo pipefail

STATE="$1"
IMAGES="$STATE/images"
BASE_IMAGE="$IMAGES/debian-12.qcow2"
OUT_IMAGE="$IMAGES/debian-12-browsers.qcow2"

if [ -f "$OUT_IMAGE" ]; then
    echo "browsers image already exists: $OUT_IMAGE"
    exit 0
fi
[ -f "$BASE_IMAGE" ] || { echo "base image missing: $BASE_IMAGE"; exit 1; }

RUN_OVERLAY="$IMAGES/.browsers-build-$$.qcow2"
SSH_KEY="$STATE/keys/browsers-build"
SEED="$STATE/seeds/browsers-build-seed.iso"
PIDFILE="$STATE/qemu-browsers-build.pid"
cleanup() {
    [ -f "$PIDFILE" ] && kill "$(cat "$PIDFILE")" 2>/dev/null || true
    rm -f "$RUN_OVERLAY" 2>/dev/null || true
}
trap cleanup EXIT

echo "==> creating overlay (resized for the browser stack; cloud-init grows the root fs)"
qemu-img create -q -f qcow2 -b "$BASE_IMAGE" -F qcow2 "$RUN_OVERLAY"
qemu-img resize "$RUN_OVERLAY" 12G

echo "==> assembling seed"
rm -f "$SSH_KEY" "$SSH_KEY.pub"
ssh-keygen -q -t ed25519 -N '' -f "$SSH_KEY"
USERDATA="$STATE/seeds/browsers-build-user-data.yaml"
cp /repo/testing/vm/cloud-init/user-data.browsers.yaml "$USERDATA"
sed -i "s|__COURT_SSH_PUBKEY__|$(cat "$SSH_KEY.pub")|" "$USERDATA"
python3 - "$USERDATA" \
    /repo/testing/vm/provision/10-base.sh \
    /repo/testing/vm/provision/20-apps.sh <<'PYEOF'
import sys
userdata, *scripts = sys.argv[1:]
text = open(userdata).read()
entries = ""
for path in scripts:
    script = open(path).read()
    indented = "\n".join("      " + line for line in script.splitlines())
    name = path.rsplit("/", 1)[-1]
    entries += (
        "  - path: /provision/" + name + "\n"
        "    permissions: '0755'\n"
        "    content: |\n"
        + indented + "\n"
    )
marker = "runcmd:"
idx = text.index(marker)
text = text[:idx] + entries + text[idx:]
open(userdata, "w").write(text)
PYEOF
# Unique instance-id so this bake boot is a distinct cloud-init instance.
METADATA="$STATE/seeds/browsers-build-meta.yaml"
echo "instance-id: browsers-build-$(date +%s%N)" > "$METADATA"
cloud-localds "$SEED" "$USERDATA" "$METADATA"

echo "==> booting (first boot installs ~1.5 GB of browsers; be patient)"
SSH_PORT=$(( 24000 + (RANDOM % 500) ))
KVM=0; [ -e /dev/kvm ] && KVM=1
bash /repo/testing/vm/qemu/boot.sh "$RUN_OVERLAY" "$SEED" "$SSH_PORT" "$KVM" "$PIDFILE"

SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
     -o ConnectTimeout=5 -o LogLevel=ERROR -i "$SSH_KEY")
bash /repo/testing/vm/qemu/wait-ssh.sh 127.0.0.1 "$SSH_PORT" court "$SSH_KEY" 1200
"${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 "sudo cloud-init status --wait >/dev/null 2>&1 || true"
"${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 \
    "for i in \$(seq 1 300); do [ -f /var/lib/ferrokey-apps-provisioned ] && exit 0; sleep 2; done; exit 1" \
    || { echo "browsers image provisioning did not complete"; exit 1; }
echo "==> provisioning complete; snapshotting the image"

# Shut the VM down cleanly, then convert the overlay to a standalone base.
"${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 "sudo poweroff" 2>/dev/null || true
sleep 5
# QEMU removes its pidfile on clean exit; guard the cat.
if [ -f "$PIDFILE" ]; then
    kill "$(cat "$PIDFILE")" 2>/dev/null || true
fi
sleep 2

qemu-img convert -O qcow2 "$RUN_OVERLAY" "$OUT_IMAGE"
rm -f "$RUN_OVERLAY"
echo "browsers image built: $OUT_IMAGE"
