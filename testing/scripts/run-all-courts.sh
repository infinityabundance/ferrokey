#!/usr/bin/env bash
# THE single court entrypoint (rule 45):
#
#   ./testing/scripts/run-all-courts.sh
#
# preflight → purge VM scratch → build images → unit/build courts (Docker)
# → clean-build court → VM courts (X11: kernel-security, systemd, soak,
# uinput, permissions, x11, focus, crash, repeat, modifiers, layouts,
# applications, dead-keys, text-mode, touch, altgr, full-desktop, sdl,
# terminal; browsers: firefox, chromium, electron; Wayland: wayland,
# xwayland) → mutation courts (§93) → evidence pull → security seal (§90,
# §91, §96) → evidence → compatibility receipt → postflight.
#
# §94: failures propagate — a court FAIL aborts the suite non-zero (no
# `|| true` masks a receipt), and run-court-inner.sh exits non-zero on a
# failed guest receipt, so CI goes RED.
set -euo pipefail
# The suite's relative `scripts/*` calls are anchored at testing/ (there is
# no top-level scripts/ dir); lib.sh resolves REPO_ROOT absolutely via
# BASH_SOURCE. New phases outside testing/ must use $REPO_ROOT explicitly.
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

# One coherent run: every child script (run-vm-court, unit/clean, evidence,
# receipt, seal) inherits the same run id and run dir. Without this, each
# child recomputes a fresh timestamp and evidence scatters across run dirs.
export RUN_ID RUN_DIR

echo "══════════════════════════════════════════════════════════════"
echo "  FERROKEY COURTS — run $RUN_ID"
echo "══════════════════════════════════════════════════════════════"

host_safety_preflight

# Purge the previous run's scratch from the VM state volume (evidence,
# payload, overlays, seeds, keys, logs) so the tmpfs-backed docker data-root
# cannot silently exhaust mid-suite (§51). The base images are kept.
echo
echo "── PURGING VM STATE SCRATCH ──"
"$DOCKER" run --rm -v ferrokey-vm-state:/court/state alpine sh -c \
    'rm -rf /court/state/evidence /court/state/payload /court/state/overlays /court/state/seeds /court/state/keys /court/state/logs; mkdir -p /court/state/evidence' \
    || echo "WARNING: could not purge the VM state volume (disk headroom may be reduced)"

echo
echo "── BUILDING COURT IMAGES ──"
bash scripts/build-images.sh

# The docker image build-cache (several GB on a fresh build) is only needed
# by `docker build`; the per-court payload builds are `docker run` and do
# not touch it. Prune it so the tmpfs-backed data-root keeps headroom for
# the VM overlays (the wayland profile grows a ~6G overlay).
echo
echo "── PRUNING DOCKER BUILD CACHE ──"
"$DOCKER" builder prune -f >/dev/null 2>&1 || true

echo
echo "── BUILD + CORE UNIT COURTS (Docker) ──"
bash scripts/run-unit-court.sh

echo
 echo "── CLEAN BUILD COURT (empty caches) ──"
 bash scripts/run-clean-court.sh

 echo
 echo "── DOCUMENTATION DRIFT COURTS (WS1/WS2) ──"
 bash scripts/architecture-drift.sh
 bash scripts/man-drift.sh

 echo
 echo "── FORMAL VERIFICATION COURTS (WS3, Kani in the ferrokey-kani VM) ──"
 bash "$REPO_ROOT/proofs/run-proofs.sh"
 bash "$REPO_ROOT/proofs/run-negative-controls.sh"

 echo
 echo "── ADAPTIVE GEOMETRY COURT (WS4) ──"
 bash "$REPO_ROOT/testing/scripts/adaptive-court.sh"

 echo
 echo "── SHELL-AWARE ROWS COURT (WS5) ──"
 bash "$REPO_ROOT/testing/scripts/shell-court.sh"

echo
 echo "── VM COURTS (X11 profile) ──"
 for court in kernel-security systemd soak socket-hijack cross-user device-lifetime uinput permissions x11 focus crash \
    repeat modifiers layouts applications dead-keys text-mode touch altgr \
    full-desktop sdl terminal terminal-workspace session-lifetime backend-selection; do
    echo
    echo "── VM court: $court ──"
    bash scripts/run-vm-court.sh "$court" x11
 done

# §66–§68: the KASAN+UBSAN+LOCKDEP instrumented-kernel court. Enabled with
# KASAN=1 (the security-court hostile entrypoint sets it); the kernel is
# built once and cached in the ferrokey-kasan-kernel volume.
if [ -n "${KASAN:-}" ]; then
    echo
    echo "── VM COURT: kernel-debug (KASAN+UBSAN+LOCKDEP kernel, §66–§68) ──"
    bash scripts/build-kasan-kernel.sh
    bash scripts/run-vm-court.sh kernel-debug x11
fi

echo
 echo "── SEC.COURT.MUTATION (§93) ──"
 MUTATION_RUN_DIR="$RUN_DIR/mutations" bash scripts/run-mutation-courts.sh

# The mutation builds repopulated the shared payload build-cache volumes.
# The browsers/wayland courts rebuild them on demand; dropping the caches
# now keeps the tmpfs-backed data-root from overflowing under the wayland
# profile's ~6G overlay.
echo
 echo "── FREEING PAYLOAD BUILD CACHES ──"
 "$DOCKER" volume rm ferrokey-payload-target ferrokey-payload-targets 2>/dev/null || true
 "$DOCKER" volume create ferrokey-payload-target >/dev/null 2>&1 || true
 "$DOCKER" volume create ferrokey-payload-targets >/dev/null 2>&1 || true

echo
 echo "── VM COURTS (browsers appliance) ──"
 # firefox chromium electron run on the pre-baked browsers image (the
 # ~1.5 GB browser stack is installed once at image-build time).
 for court in firefox chromium electron; do
    echo
    echo "── VM court: $court ──"
    bash scripts/run-vm-court.sh "$court" x11 debian-12-browsers
 done

 echo
 echo "── VM COURTS (Wayland profile) ──"
 for court in wayland xwayland; do
    echo
    echo "── VM court: $court ──"
    bash scripts/run-vm-court.sh "$court" wayland
 done

echo
echo "── PULLING VM EVIDENCE INTO THE RUN DIR ──"
# Every VM court's evidence lives in the shared state volume; copy it into
# the run dir so the evidence seal + hash cover the receipts, manifests,
# sandbox probes and dmesg captures (§91).
"$DOCKER" run --rm -v ferrokey-vm-state:/court/state -v "$RUN_DIR:/out" \
    alpine sh -c '
        for d in /court/state/evidence/*/; do
            [ -d "$d" ] || continue
            court=$(basename "$d")
            mkdir -p "/out/courts/$court"
            cp -a "$d"/. "/out/courts/$court/" 2>/dev/null || true
        done
    '

echo
echo "── COLLECTING EVIDENCE ──"
bash scripts/collect-evidence.sh

echo
echo "── COMPATIBILITY RECEIPT (§37) ──"
bash scripts/generate-compat-receipt.sh "$RUN_ID"

echo
echo "── POSTFLIGHT ──"
host_safety_postflight

echo
echo "── SECURITY EVIDENCE SEAL (§90, §91, §96) ──"
# Runs after the postflight so the host-contamination verdict exists for
# the seal's HOST_CONTAMINATION gate.
bash scripts/seal-security-evidence.sh "$RUN_ID"
# The seal's outputs were written after collect-evidence; fold them into
# the sealed evidence hash.
( cd "$RUN_DIR" && sha256sum security-summary.json security-receipt.md security-manifest.json \
   >> evidence.sha256 2>/dev/null || true )

echo
echo "══════════════════════════════════════════════════════════════"
echo "  COURTS COMPLETE — evidence: $RUN_DIR"
echo "══════════════════════════════════════════════════════════════"
