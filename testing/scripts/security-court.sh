#!/usr/bin/env bash
# §97 — the high-intensity hostile audit entrypoint:
#
#   ./testing/scripts/security-court.sh --hostile
#
# (the repository has no xtask crate; this script is the `cargo xtask
# security-court --hostile` equivalent).
#
# It runs the complete hostile suite inside disposable VMs:
#   build clean
#   start disposable VM
#   boot hardened/debug kernel          (kernel-debug court, §66–§68)
#   start constrained broker
#   verify privilege state / seccomp / FD inventory / device inventory
#   fuzz protocol + state transitions
#   stress event path
#   attempt forbidden syscalls/devices/network/ioctl/device recreation
#   SIGKILL components, restart broker
#   inspect kernel logs
#   mutation courts (§93)
#   collect receipts, hash evidence
#   destroy passing overlays, retain failing overlays
#   fail non-zero if any receipt fails
#
# The full pipeline is `run-all-courts.sh` (the single court entrypoint,
# rule 45) followed by the security evidence seal. Every failed receipt
# propagates to this script's exit code (§94).
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

MODE="${1:---hostile}"
case "$MODE" in
    --hostile)
        RUN_ID="${RUN_ID:-sec-$(date -u +%Y%m%dT%H%M%SZ)}"
        export RUN_ID
        # The hostile audit boots the KASAN+UBSAN+LOCKDEP kernel court (§66–§68).
        export KASAN=1
        echo "══════════════════════════════════════════════════════════════"
        echo "  FERROKEY SECURITY COURT (hostile) — run $RUN_ID"
        echo "══════════════════════════════════════════════════════════════"
        bash scripts/run-all-courts.sh
        echo
        echo "── SECURITY EVIDENCE SEAL ──"
        bash scripts/seal-security-evidence.sh "$RUN_ID"
        ;;
    *)
        echo "usage: security-court.sh [--hostile]"
        exit 2
        ;;
esac
