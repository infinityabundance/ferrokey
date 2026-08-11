#!/usr/bin/env bash
# Full VM court lifecycle, run INSIDE the oracle container.
#
# usage: run-court-inner.sh <court-name> <profile: x11|wayland>
#
# Paths inside the container:
#   /repo            the repository (read-only)
#   /court/state     writable state volume (images, overlays, payload, evidence)
set -euo pipefail

COURT="$1"
PROFILE="$2"

STATE=/court/state
IMAGES=$STATE/images
OVERLAYS=$STATE/overlays
PAYLOAD=$STATE/payload
mkdir -p "$IMAGES" "$OVERLAYS" "$PAYLOAD" "$STATE/logs" "$STATE/keys" "$STATE/seeds"

DISTRO="${DISTRO:-debian-12}"
BASE_IMAGE="$IMAGES/$DISTRO.qcow2"

# ---------------------------------------------------------------------------
# 1. Immutable base image (rule 5): download once, hash it.
# ---------------------------------------------------------------------------
if [ ! -f "$BASE_IMAGE" ]; then
    echo "downloading base image $DISTRO"
    case "$DISTRO" in
        debian-12)
            curl -fL -o "$BASE_IMAGE.part" \
                https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-genericcloud-amd64.qcow2
            mv "$BASE_IMAGE.part" "$BASE_IMAGE"
            ;;
        *)
            echo "unknown distro $DISTRO"
            exit 1
            ;;
    esac
fi
BASE_SHA=$(sha256sum "$BASE_IMAGE" | cut -d' ' -f1)
echo "base image sha256=$BASE_SHA"

# ---------------------------------------------------------------------------
# 2. Disposable overlay (rule 6): every run starts from known state.
# ---------------------------------------------------------------------------
RUN_OVERLAY="$OVERLAYS/$COURT-$(date +%s%N).qcow2"
qemu-img create -q -f qcow2 -b "$BASE_IMAGE" -F qcow2 "$RUN_OVERLAY"

# ---------------------------------------------------------------------------
# 3. SSH key + cloud-init seed (rule 7: self-provisioning, no manual config).
# ---------------------------------------------------------------------------
SSH_KEY="$STATE/keys/court-$(date +%s%N)"
ssh-keygen -q -t ed25519 -N '' -f "$SSH_KEY"
PUBKEY=$(cat "$SSH_KEY.pub")

USERDATA="$STATE/seeds/$COURT-user-data.yaml"
cp "/repo/testing/vm/cloud-init/user-data.$PROFILE.yaml" "$USERDATA"
sed -i "s|__COURT_SSH_PUBKEY__|$PUBKEY|" "$USERDATA"
# Insert the base provision script into the existing write_files section.
python3 - "$USERDATA" /repo/testing/vm/provision/10-base.sh <<'PYEOF'
import sys
userdata, script_path = sys.argv[1], sys.argv[2]
script = open(script_path).read()
indented = "\n".join("      " + line for line in script.splitlines())
entry = (
    "  - path: /provision/10-base.sh\n"
    "    permissions: '0755'\n"
    "    content: |\n"
    + indented + "\n"
)
text = open(userdata).read()
# Insert after the last existing write_files entry (before runcmd).
marker = "runcmd:"
idx = text.index(marker)
text = text[:idx] + entry + text[idx:]
open(userdata, "w").write(text)
PYEOF

SEED="$STATE/seeds/$COURT-seed.iso"
cloud-localds "$SEED" "$USERDATA"

# ---------------------------------------------------------------------------
# 4. Boot (rule 8: headless; rule 36: KVM when available, TCG fallback).
# ---------------------------------------------------------------------------
SSH_PORT=$(( 22000 + (RANDOM % 1000) ))
PIDFILE="$STATE/qemu-$COURT.pid"
KVM=0; [ -e /dev/kvm ] && KVM=1
bash /repo/testing/vm/qemu/boot.sh "$RUN_OVERLAY" "$SEED" "$SSH_PORT" "$KVM" "$PIDFILE"

cleanup() {
    if [ -f "$PIDFILE" ]; then
        kill "$(cat "$PIDFILE")" 2>/dev/null || true
        sleep 1
        kill -9 "$(cat "$PIDFILE")" 2>/dev/null || true
    fi
    rm -f "$RUN_OVERLAY" 2>/dev/null || true
}
trap cleanup EXIT

bash /repo/testing/vm/qemu/wait-ssh.sh 127.0.0.1 "$SSH_PORT" court "$SSH_KEY" 600

SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
     -o ConnectTimeout=5 -o LogLevel=ERROR -i "$SSH_KEY")
SCP=(scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i "$SSH_KEY" -P "$SSH_PORT")

# ---------------------------------------------------------------------------
# 5. Push the payload (rule 9: real binaries, real kernel path).
# ---------------------------------------------------------------------------
"${SCP[@]}" -r "$PAYLOAD"/. "court@127.0.0.1:payload/" 2>/dev/null || {
    "${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 "mkdir -p payload"
    "${SCP[@]}" -r "$PAYLOAD"/. "court@127.0.0.1:payload/"
}
"${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 "chmod +x payload/* 2>/dev/null; mkdir -p court-output"

# ---------------------------------------------------------------------------
# 6. Run the court (rules 9-29). The court scripts run inside the VM and
#    write a receipt to ~/court-output.
# ---------------------------------------------------------------------------
if [ -f "/repo/testing/courts/$COURT/court.sh" ]; then
    "${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 \
        "cd ~/payload && sudo env RUN_ID=vm bash courts/$COURT/court.sh" \
        || echo "court script exit code: $?"
else
    echo "no court script for $COURT"
fi

# ---------------------------------------------------------------------------
# 7. Collect evidence (rules 38-40): receipts, logs, device dumps.
# ---------------------------------------------------------------------------
EVIDENCE="$STATE/evidence/$COURT"
mkdir -p "$EVIDENCE"/{logs,devices,screenshots}
"${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 "sudo cp /proc/bus/input/devices /home/court/court-output/devices.txt 2>/dev/null; sudo udevadm info --export-db > /home/court/court-output/udev.txt 2>/dev/null || true" || true
"${SCP[@]}" -r "court@127.0.0.1:court-output/." "$EVIDENCE/" 2>/dev/null || true
cp "$STATE/logs/qemu-$SSH_PORT.log" "$EVIDENCE/logs/qemu.log" 2>/dev/null || true

# The receipt decides PASS/FAIL.
RESULT="FAIL"
if [ -f "$EVIDENCE/receipt.json" ]; then
    RESULT=$(jq -r .result "$EVIDENCE/receipt.json")
fi
echo "COURT $COURT RESULT: $RESULT"
echo "$RESULT" > "$EVIDENCE/result"
sha256sum "$BASE_IMAGE" > "$EVIDENCE/vm-image.sha256"
echo "{\"court\":\"$COURT\",\"result\":\"$RESULT\",\"vm_image_sha256\":\"$BASE_SHA\",\"kernel\":\"$(echo unknown)\"}" > "$STATE/evidence/$COURT.meta.json"

# ---------------------------------------------------------------------------
# 8. Destroy the overlay (rule 40): success destroys, failure retains.
# ---------------------------------------------------------------------------
if [ "$RESULT" = "PASS" ]; then
    rm -f "$RUN_OVERLAY"
    echo "overlay destroyed (PASS)"
else
    cp "$RUN_OVERLAY" "$EVIDENCE/failed-overlay.qcow2" 2>/dev/null || true
    echo "overlay retained as failure evidence"
fi

exit 0
