#!/usr/bin/env bash
# Base provisioning inside the VM (rule 7): everything a court needs beyond
# the cloud-init package list. Runs as root on first boot.
set -euo pipefail

# Load the uinput module (the cloud images do not auto-load it) and make it
# persist. The court's virtual keyboard lives on the GUEST kernel — the host
# kernel/modules are never touched (rules 1, 3).
modprobe uinput 2>/dev/null || true
echo uinput > /etc/modules-load.d/ferrokey-uinput.conf

# A stable /dev/uinput permissions baseline for the permissions court:
# root-only by default on Debian — record what we have.
stat -c '%A %U:%G %n' /dev/uinput > /etc/ferrokey-uinput-perms.txt 2>/dev/null || \
    echo "no /dev/uinput" > /etc/ferrokey-uinput-perms.txt

# Allow the court user to run a couple of diagnostic tools without a tty.
echo 'court ALL=(ALL) NOPASSWD: /usr/bin/evtest, /usr/bin/udevadm' > /etc/sudoers.d/court-tools
chmod 440 /etc/sudoers.d/court-tools

# ── Phase 3: the dedicated unprivileged broker identity (§3) ──────────────
# The runtime broker (`ferrokeyd serve`) runs as this user with zero
# capabilities; it is never root. The socket lives in a runtime directory
# owned by this user (the daemon refuses group/world-writable parents, §26).
if ! id -u ferrokeyd >/dev/null 2>&1; then
    useradd --system --home-dir /nonexistent --shell /usr/sbin/nologin ferrokeyd
fi
mkdir -p /run/ferrokeyd
chown ferrokeyd:ferrokeyd /run/ferrokeyd
chmod 0755 /run/ferrokeyd

# Mark provisioning complete.
touch /var/lib/ferrokey-provisioned
echo "base provisioning complete"
