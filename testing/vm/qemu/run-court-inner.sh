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
            # NOTE: the *generic* image, not genericcloud — the trimmed cloud
            # kernel (linux-image-cloud-amd64) ships no uinput module, so the
            # court VM could never create the virtual keyboard.
            curl -fL -o "$BASE_IMAGE.part" \
                https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-generic-amd64.qcow2
            mv "$BASE_IMAGE.part" "$BASE_IMAGE"
            ;;
        debian-12-genericcloud)
            curl -fL -o "$BASE_IMAGE.part" \
                https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-genericcloud-amd64.qcow2
            mv "$BASE_IMAGE.part" "$BASE_IMAGE"
            ;;
        ubuntu-24-04)
            curl -fL -o "$BASE_IMAGE.part" \
                https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img
            mv "$BASE_IMAGE.part" "$BASE_IMAGE"
            ;;
        fedora-40)
            curl -fL -o "$BASE_IMAGE.part" \
                https://download.fedoraproject.org/pub/fedora/linux/releases/40/Cloud/x86_64/images/Fedora-Cloud-Base-Generic.x86_64-40-1.14.qcow2
            mv "$BASE_IMAGE.part" "$BASE_IMAGE"
            ;;
        arch-current)
            curl -fL -o "$BASE_IMAGE.part" \
                https://geo.mirror.pkgbuild.com/images/latest/Arch-Linux-x86_64-cloudimg.qcow2
            mv "$BASE_IMAGE.part" "$BASE_IMAGE"
            ;;
        debian-12-browsers)
            # Pre-baked appliance (rule 5): the X11 stack plus Firefox,
            # Chromium, SDL and Electron, built ONCE and cached. Built on
            # demand by the qemu builder; see build-browsers-image.sh.
            bash /repo/testing/vm/qemu/build-browsers-image.sh "$STATE"
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
# The wayland profile installs KWin (large KDE dependency tree); grow the
# root fs so the install fits. cloud-init auto-grows the partition on boot.
# 6G keeps the COW data bounded (the state volume is a 26G tmpfs shared by
# all court evidence).
if [ "$PROFILE" = "wayland" ]; then
    qemu-img resize "$RUN_OVERLAY" 6G
fi

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
# A fresh instance-id per boot (rule 7): cloud-init re-runs the user-data for
# the new instance, re-applying the SSH key. Without this, an overlay booted
# over a PRE-BAKED base image (debian-12-browsers) looks like the same
# instance cloud-init already saw, so the new court key is never installed
# and SSH never authenticates.
METADATA="$STATE/seeds/$COURT-meta.yaml"
echo "instance-id: $COURT-$(date +%s%N)" > "$METADATA"
cloud-localds "$SEED" "$USERDATA" "$METADATA"

# ---------------------------------------------------------------------------
# 4. Boot (rule 8: headless; rule 36: KVM when available, TCG fallback).
# ---------------------------------------------------------------------------
SSH_PORT=$(( 22000 + (RANDOM % 1000) ))
PIDFILE="$STATE/qemu-$COURT.pid"
KVM=0; [ -e /dev/kvm ] && KVM=1
if [ -n "${KASAN:-}" ]; then
    # §66–§68: boot the KASAN+UBSAN+LOCKDEP kernel by direct -kernel boot
    # (built once by build-kasan-kernel.sh and cached in the kasan volume).
    BZIMAGE=/kasan-kernel/bzImage
    if [ ! -f "$BZIMAGE" ]; then
        echo "KASAN kernel missing: run testing/scripts/build-kasan-kernel.sh"
        exit 1
    fi
    bash /repo/testing/vm/qemu/boot-kernel.sh "$RUN_OVERLAY" "$SEED" "$SSH_PORT" "$KVM" "$PIDFILE" "$BZIMAGE"
else
    bash /repo/testing/vm/qemu/boot.sh "$RUN_OVERLAY" "$SEED" "$SSH_PORT" "$KVM" "$PIDFILE"
fi

cleanup() {
    # QEMU removes its pidfile on clean exit, so each cat is guarded
    # (kill after the file is gone would print a confusing error).
    if [ -f "$PIDFILE" ]; then
        kill "$(cat "$PIDFILE")" 2>/dev/null || true
        sleep 1
    fi
    if [ -f "$PIDFILE" ]; then
        kill -9 "$(cat "$PIDFILE")" 2>/dev/null || true
    fi
    rm -f "$RUN_OVERLAY" 2>/dev/null || true
}
trap cleanup EXIT

bash /repo/testing/vm/qemu/wait-ssh.sh 127.0.0.1 "$SSH_PORT" court "$SSH_KEY" 600

SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
     -o ConnectTimeout=5 -o LogLevel=ERROR -i "$SSH_KEY")
SCP=(scp -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i "$SSH_KEY" -P "$SSH_PORT")

# First boot runs cloud-init (package install + provision scripts). sshd comes
# up before that finishes, so wait for cloud-init to complete — the courts
# depend on jq/openbox/evtest/libinput being present.
echo "waiting for cloud-init to finish..."
"${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 "sudo cloud-init status --wait >/dev/null 2>&1 || true"
"${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 \
    "for i in \$(seq 1 120); do [ -f /var/lib/ferrokey-provisioned ] && exit 0; sleep 1; done; exit 1" \
    || { echo "provisioning did not complete"; exit 1; }
echo "cloud-init complete; provisioned"

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
    # The court script runs as the unprivileged `court` user; privileged steps
    # (ferrokeyd, evtest, …) elevate per-command with sudo inside lib.sh. This
    # keeps the SO_PEERCRED whitelist meaningful: clients are uid 1000.
    "${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 \
        "cd ~/payload && env RUN_ID=vm MUTATION=\"${MUTATION:-}\" SOAK_SECONDS=\"${SOAK_SECONDS:-}\" bash courts/$COURT/court.sh" \
        || echo "court script exit code: $?"
else
    echo "no court script for $COURT"
fi

# ---------------------------------------------------------------------------
# 7. Collect evidence (rules 38-40): receipts, logs, device dumps.
# ---------------------------------------------------------------------------
EVIDENCE="$STATE/evidence/$COURT"
# §94: a court that dies before finish_court (set -e, crash) must NEVER read
# a stale PASS receipt from an earlier run — clear the whole evidence dir so
# a missing receipt defaults to FAIL instead of inheriting yesterday's PASS.
rm -rf "$EVIDENCE"
mkdir -p "$EVIDENCE"/{logs,devices,screenshots}
"${SSH[@]}" -p "$SSH_PORT" court@127.0.0.1 "sudo cp /proc/bus/input/devices /home/court/court-output/devices.txt 2>/dev/null; sudo udevadm info --export-db > /home/court/court-output/udev.txt 2>/dev/null || true; sudo cp /var/log/Xorg.0.log /home/court/court-output/xorg-full.log 2>/dev/null || true" || true
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

# §94: a failed receipt must fail the pipeline — the oracle container exit
# code carries the court result so run-vm-court.sh and run-all-courts.sh
# propagate it to CI. No `|| true`, no unconditional `exit 0` may mask a
# security failure.
if [ "$RESULT" = "PASS" ]; then
    exit 0
fi
exit 1
