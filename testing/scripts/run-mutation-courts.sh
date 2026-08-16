#!/usr/bin/env bash
# SEC.COURT.MUTATION (§93) — deliberate security-regression mutations.
#
# For each mutation kind, this builds a disposable MUTATED COPY of the repo
# (production source is never touched), boots a disposable VM, and runs the
# kernel-security court in MUTATION mode. The court verdict must be FAIL, and
# check-mutation.py proves the failure is on EXACTLY the gate(s) that
# mutation breaks — proving the court actually guards each property.
#
#   ./testing/scripts/run-mutation-courts.sh            # all six kinds
#   MUTATION_KINDS="keep-caps allow-inet" ./testing/scripts/run-mutation-courts.sh
#
# Mutations: run-as-root keep-caps no-nnp allow-inet allow-ioctl allow-openat
# (see mutations.py for the exact regression each one applies).
#
# Evidence: per-kind court evidence is written under MUTATION_RUN_DIR/$kind/
# (default: testing/evidence/mut-<kind>-<ts>/). Any mutation that is NOT
# caught (court PASSed, or the wrong gate failed) makes this script exit
# non-zero: SEC.COURT.MUTATION fails the pipeline.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

DEFAULT_KINDS="run-as-root keep-caps no-nnp allow-inet allow-ioctl allow-openat"
KINDS=(${MUTATION_KINDS:-$DEFAULT_KINDS})
MUTATION_RUN_DIR="${MUTATION_RUN_DIR:-}"
# Docker mounts `-v` sources from the DAEMON's working directory; a relative
# path is interpreted as a volume name (and rejected for containing '/').
# Resolve the run dir to an absolute path so the evidence pull always works.
if [ -n "$MUTATION_RUN_DIR" ]; then
    mkdir -p "$MUTATION_RUN_DIR"
    MUTATION_RUN_DIR="$(cd "$MUTATION_RUN_DIR" && pwd)"
fi

host_safety_preflight

# The mutation builds must NEVER share the production cargo target volumes:
# they compile a deliberately mutated copy of the repo mounted at the same
# /repo path, so cargo fingerprinting cannot distinguish it from the real
# tree — a mutated binary would then be reused by later court runs (the
# session-lifetime court once ran with the allow-openat-mutated ferrokeyd
# and reported every openat allowed). Isolate the mutations onto their own
# volumes, recreated so each suite starts from empty caches.
export PAYLOAD_TARGET_VOLUME="ferrokey-payload-target-mut"
export PAYLOAD_TARGETS_VOLUME="ferrokey-payload-targets-mut"
"$DOCKER" volume rm -f ferrokey-payload-target-mut ferrokey-payload-targets-mut >/dev/null 2>&1 || true
"$DOCKER" volume create ferrokey-payload-target-mut >/dev/null
"$DOCKER" volume create ferrokey-payload-targets-mut >/dev/null

# The mutation runs overwrite /court/state/evidence/kernel-security (they
# boot the same court name in MUTATION mode, and their failing receipts must
# stay visible for check-mutation.py). Snapshot the CLEAN run's evidence AND
# its meta.json first and restore both after every mutation so the suite's
# later evidence pull + security seal (§96) still read the non-mutated court
# record (the meta.json is otherwise left pointing at the last mutation's
# FAIL verdict).
"$DOCKER" run --rm -v ferrokey-vm-state:/court/state alpine sh -c \
    'rm -rf /court/state/evidence/kernel-security.snapshot; \
     if [ -d /court/state/evidence/kernel-security ]; then \
         cp -a /court/state/evidence/kernel-security /court/state/evidence/kernel-security.snapshot; \
     fi; \
     if [ -f /court/state/evidence/kernel-security.meta.json ]; then \
         cp -a /court/state/evidence/kernel-security.meta.json /court/state/evidence/kernel-security.meta.json.snapshot; \
     fi'

WORK=""
RUN_IDS=()
cleanup() {
    # The trap's last command must succeed: in bash ≥ 5.3 an EXIT trap whose
    # final command fails overrides the script's exit status, silently
    # turning `exit 0` into a non-zero exit — which abort-ed the whole suite
    # at the mutation phase (§94 would then be a false negative).
    if [ -n "$WORK" ]; then
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT

failed=0
for kind in "${KINDS[@]}"; do
    echo
    echo "══════════════════════════════════════════════════════════"
    echo "  SEC.COURT.MUTATION — mutation: $kind"
    echo "══════════════════════════════════════════════════════════"

    # ── 1. Disposable mutated copy (never the production tree) ──────────
    # The tar carries SOURCE only: .kani-work (the kani verifier's work dir,
    # GBs of root-owned build artifacts) and every build target dir are
    # excluded so the copy stays small and lands on the /tmp tmpfs.
    WORK=$(mktemp -d /tmp/ferrokey-mutation.XXXXXX)
    echo "mutated copy: $WORK/src"
    mkdir -p "$WORK/src"
    tar --exclude=./target --exclude=./.kani-work --exclude=./testing/evidence \
        --exclude=./testing/targets/target \
        -C "$REPO_ROOT" -cf - . | tar -C "$WORK/src" -xf -
    python3 "$REPO_ROOT/testing/scripts/mutations.py" "$kind" "$WORK/src"

    # ── 2. Boot the VM and run the kernel-security court in MUTATION mode ─
    # The court MUST FAIL (the mutation must be caught); run-vm-court.sh
    # propagates the guest receipt (§94) as its exit code.
    MUT_RUN_ID="mut-$kind-$(date -u +%Y%m%dT%H%M%SZ)"
    RUN_IDS+=("$MUT_RUN_ID")
    if [ -n "$MUTATION_RUN_DIR" ]; then
        KIND_DIR="$MUTATION_RUN_DIR/$kind"
    else
        KIND_DIR="$EVIDENCE_DIR/$MUT_RUN_ID"
    fi
    mkdir -p "$KIND_DIR/courts"
    set +e
    RUN_ID="$MUT_RUN_ID" \
    PAYLOAD_DIR="$WORK/payload" \
    REPO_ROOT="$WORK/src" \
    MUTATION="$kind" \
        bash "$REPO_ROOT/testing/scripts/run-vm-court.sh" kernel-security x11
    rc=$?
    set -e

    if [ "$rc" -eq 0 ]; then
        echo "MUTATION $kind: court PASSED — mutation NOT caught (FAIL)"
        failed=1
        continue
    fi
    echo "MUTATION $kind: court FAILed as required (rc=$rc)"

    # ── 3. Pull the per-court evidence from the VM state volume ──────────
    "$DOCKER" run --rm -v ferrokey-vm-state:/court/state \
        -v "$KIND_DIR:/out" alpine sh -c '
            mkdir -p /out/courts
            if [ -d /court/state/evidence/kernel-security ]; then
                cp -r /court/state/evidence/kernel-security /out/courts/
            fi
            cp /court/state/evidence/*.meta.json /out/courts/ 2>/dev/null || true
        '

    # ── 4. Verify the failure is on EXACTLY the broken gate(s) ────────────
    if python3 "$REPO_ROOT/testing/scripts/check-mutation.py" "$kind" \
            "$KIND_DIR/courts/kernel-security"; then
        echo "MUTATION $kind: caught by the correct gate(s) — PASS"
        # The retained failed overlay is boot evidence but costs ~1 GB on the
        # state tmpfs; the assertions/receipts are the authoritative record.
        "$DOCKER" run --rm -v ferrokey-vm-state:/court/state alpine sh -c \
            'rm -f /court/state/evidence/kernel-security/failed-overlay.qcow2'
    else
        echo "MUTATION $kind: caught, but NOT by the expected gate(s) — FAIL"
        failed=1
    fi

    # ── 5. Restore the clean kernel-security evidence (see the snapshot
    #        above): the mutation's failing record is already preserved under
    #        $KIND_DIR, so the volume can go back to the non-mutated court
    #        record for the suite's evidence pull and security seal. The
    #        snapshot is COPIED back (never moved): every mutation restores
    #        the same clean record.
    "$DOCKER" run --rm -v ferrokey-vm-state:/court/state alpine sh -c \
        'rm -rf /court/state/evidence/kernel-security; \
         if [ -d /court/state/evidence/kernel-security.snapshot ]; then \
             cp -a /court/state/evidence/kernel-security.snapshot /court/state/evidence/kernel-security; \
         fi; \
         rm -f /court/state/evidence/kernel-security.meta.json; \
         if [ -f /court/state/evidence/kernel-security.meta.json.snapshot ]; then \
             cp -a /court/state/evidence/kernel-security.meta.json.snapshot /court/state/evidence/kernel-security.meta.json; \
         fi'

    rm -rf "$WORK"
    WORK=""
done

echo
RESULTS_DIR="${MUTATION_RUN_DIR:-$EVIDENCE_DIR}"
mkdir -p "$RESULTS_DIR"
if [ "$failed" -ne 0 ]; then
    echo "SEC.COURT.MUTATION ............ FAIL (one or more mutations not caught)"
    python3 - "$RESULTS_DIR/results.json" "${RUN_IDS[*]}" <<'EOF'
import json, os, sys
with open(sys.argv[1], "w") as fh:
    json.dump({"all_caught": False, "evidence": sys.argv[2].split()}, fh)
EOF
    fail "mutation" "phase" "mutation"
    exit 1
fi
echo "SEC.COURT.MUTATION ............ PASS (all ${#KINDS[@]} mutations caught)"
echo "evidence: ${RUN_IDS[*]}"
# Machine-readable summary for the security seal (§93, §96).
python3 - "$RESULTS_DIR/results.json" "${KINDS[*]}" "${RUN_IDS[*]}" <<'EOF'
import json, sys
with open(sys.argv[1], "w") as fh:
    json.dump({
        "all_caught": True,
        "kinds": sys.argv[2].split(),
        "evidence": sys.argv[3].split(),
    }, fh, indent=2)
EOF
pass "mutation" "phase" "mutation"
exit 0
