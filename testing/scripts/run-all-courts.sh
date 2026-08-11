#!/usr/bin/env bash
# THE single court entrypoint (rule 45):
#
#   ./testing/scripts/run-all-courts.sh
#
# preflight → build images → unit/build courts (Docker) → clean-build court
# → VM courts (disposable QEMU VMs: uinput, permissions, x11, focus, crash,
# repeat, modifiers, layouts, applications, wayland, xwayland) → evidence →
# postflight.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

echo "══════════════════════════════════════════════════════════════"
echo "  FERROKEY COMPATIBILITY COURTS — run $RUN_ID"
echo "══════════════════════════════════════════════════════════════"

host_safety_preflight

echo
echo "── BUILDING COURT IMAGES ──"
bash scripts/build-images.sh

echo
echo "── BUILD + CORE UNIT COURTS (Docker) ──"
bash scripts/run-unit-court.sh

echo
echo "── CLEAN BUILD COURT (empty caches) ──"
bash scripts/run-clean-court.sh || { echo "CLEAN BUILD COURT FAIL"; exit 1; }

echo
echo "── VM COURTS (X11 profile) ──"
for court in uinput permissions x11 focus crash repeat modifiers layouts applications; do
    echo
    echo "── VM court: $court ──"
    bash scripts/run-vm-court.sh "$court" x11 || true
done

echo
echo "── VM COURTS (Wayland profile) ──"
for court in wayland xwayland; do
    echo
    echo "── VM court: $court ──"
    bash scripts/run-vm-court.sh "$court" wayland || true
done

echo
echo "── COLLECTING EVIDENCE ──"
bash scripts/collect-evidence.sh

echo
echo "── POSTFLIGHT ──"
host_safety_postflight

echo
echo "══════════════════════════════════════════════════════════════"
echo "  COURTS COMPLETE — evidence: $RUN_DIR"
echo "══════════════════════════════════════════════════════════════"
