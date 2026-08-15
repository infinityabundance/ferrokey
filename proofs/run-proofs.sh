#!/usr/bin/env bash
# Run every Kani proof over the production ferrokey-core state machine and
# emit proofs/kani-receipt.json (KANI.RECEIPT.001).
#
# The verification runs ENTIRELY inside the ferrokey-kani container (rule:
# no test tooling on the host). The production workspace is mounted
# read-only; a disposable copy is made inside the container, where ONLY the
# `rust-version` declaration is adjusted to the kani bundled toolchain
# (rustc 1.93-nightly, below the workspace MSRV declaration). The field does
# not affect codegen — the verified artifact is byte-identical to production
# — and the receipt records this explicitly.
#
#   bash proofs/run-proofs.sh
set -euo pipefail
cd "$(dirname "$0")/.."
source testing/scripts/lib.sh
sanitize_env

RECEIPT="$REPO_ROOT/proofs/kani-receipt.json"
COMMIT=$(git -C "$REPO_ROOT" rev-parse HEAD)
KANI_VERSION=$("$DOCKER" run --rm "$KANI_IMAGE" kani --version 2>/dev/null | awk '{print $2}' || echo unknown)

# OOM guardrails (rule: the verifier runs are bounded, the host survives a
# pathological proof):
#   * the container gets a hard memory cap — CBMC's solver can eat tens of GB
#     on a large VCC set, and an unbounded container would OOM the host;
#   * a per-harness timeout turns a hung proof into a FAIL, not a zombie;
#   * the docker data-root lives on a tmpfs at /run — preflight its headroom.
KANI_MEM_LIMIT="${KANI_MEM_LIMIT:-48g}"
KANI_HARNESS_TIMEOUT="${KANI_HARNESS_TIMEOUT:-45m}"
DATA_ROOT=$("$DOCKER" info --format '{{.DockerRootDir}}' 2>/dev/null || echo /run)
DATA_ROOT_FREE=$(df -P "$DATA_ROOT" 2>/dev/null | awk 'NR==2 {print $4}')
if [ -n "$DATA_ROOT_FREE" ] && [ "$DATA_ROOT_FREE" -lt 6291456 ]; then
    echo "ERROR: docker data-root ($DATA_ROOT) has < 6G free ($DATA_ROOT_FREE KiB) —"
    echo "       the verification writes GBs of codegen artifacts to the tmpfs."
    echo "       Free space (prune volumes/images) and re-run."
    exit 1
fi

if ! "$DOCKER" image inspect "$KANI_IMAGE" >/dev/null 2>&1; then
    bash testing/scripts/build-images.sh
fi

echo "── running Kani proofs (ferrokey-kani container, mem cap $KANI_MEM_LIMIT) ──"
# /work (source copy + cargo target) is a bind mount on the real disk: the
# verification writes GBs of codegen artifacts, which must not land on the
# tmpfs-backed docker data-root. The cargo registry is read from the host's
# crate cache (read-only; only crate sources).
"$DOCKER" run --rm \
    --memory "$KANI_MEM_LIMIT" \
    --memory-swap "$KANI_MEM_LIMIT" \
    -e KANI_HARNESS_TIMEOUT="$KANI_HARNESS_TIMEOUT" \
    -v "$REPO_ROOT:/repo:ro" \
    -v "$REPO_ROOT/.kani-work:/work" \
    -v "$HOME/.cargo/registry:/usr/local/cargo/registry:ro" \
    -e CARGO_HOME=/usr/local/cargo \
    -e CARGO_TARGET_DIR=/work/target \
    -e DISPLAY= -e WAYLAND_DISPLAY= -e XDG_RUNTIME_DIR=/tmp \
    -e DBUS_SESSION_BUS_ADDRESS= -e XAUTHORITY= -e SSH_AUTH_SOCK= \
    "$KANI_IMAGE" \
    bash -c '
        set -euo pipefail
        rm -rf /work/src /work/target
        mkdir -p /work/src
        tar -C /repo --exclude=./.git --exclude=./target --exclude=./testing/evidence --exclude=./.kani-work \
            -cf - . | tar -C /work/src -xf -
        # Metadata-only adjustment (see the script header).
        sed -i "s/^rust-version = \"1.96\"/rust-version = \"1.93\"/" /work/src/Cargo.toml
        cd /work/src
        cargo kani -p ferrokey-proofs -Z unstable-options \
            --harness-timeout "$KANI_HARNESS_TIMEOUT" \
            --cbmc-args --unwind 33 --unwinding-assertions
    ' 2>&1 | tee /tmp/kani-run.log

# Per-harness results: kani prints "Checking harness <name>" then
# "VERIFICATION:- SUCCESSFUL|FAILED".
python3 - /tmp/kani-run.log "$RECEIPT" "$COMMIT" "$KANI_VERSION" <<'PYEOF'
import json, re, sys

log, receipt_path, commit, kani_version = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]

# harness name -> proof id (the explicit mapping; adding a proof requires
# registering it here so the receipt and the drift court see it).
PROOFS = {
    "kani_held_unique": "KANI.HELD.001",
    "kani_release_valid": "KANI.RELEASE.001",
    "kani_repeat_invariants": "KANI.REPEAT.001",
    "kani_rollover_held_bound": "KANI.ROLLOVER.001",
    "kani_release_all_complete": "KANI.RELEASEALL.001",
    "kani_latch_semantics": "KANI.LATCH.001",
    "kani_lock_semantics": "KANI.LOCK.001",
    "kani_sequence_invariants": "KANI.SEQUENCE.001",
}

results = {}
current = None
with open(log) as fh:
    for line in fh:
        m = re.search(r"Checking harness ([A-Za-z0-9_]+::)?(\w+)", line)
        if m:
            # kani prints the fully qualified name (`proofs::kani_held_unique`);
            # the PROOFS table is keyed by the bare harness name.
            current = m.group(2)
        m = re.search(r"VERIFICATION:-\s*(SUCCESSFUL|FAILED)", line)
        if m and current:
            results.setdefault(current, m.group(1))

proofs = []
missing = []
for harness, pid in PROOFS.items():
    r = results.get(harness)
    if r is None:
        missing.append(harness)
        continue
    proofs.append({"id": pid, "harness": harness, "result": "PASS" if r == "SUCCESSFUL" else "FAIL"})

all_pass = all(p["result"] == "PASS" for p in proofs) and not missing
receipt = {
    "tool": "kani",
    "tool_version": kani_version,
    "rustc": "1.93.0-nightly (2025-11-21, kani 0.67 bundled)",
    "commit": commit,
    "verification_note": (
        "verified inside the ferrokey-kani container in a disposable copy "
        "with only the rust-version declaration adjusted to the kani bundled "
        "toolchain; the verified code is byte-identical to production (the "
        "field does not affect codegen). Loops are bounded by --unwind 33 "
        "with --unwinding-assertions: every verified loop has a constant "
        "trip bound <= 32 (the fixed-capacity core), so any loop exceeding "
        "the bound fails the proof loudly instead of truncating silently."
    ),
    "proofs": sorted(proofs, key=lambda p: p["id"]),
    "result": "PASS" if all_pass else "FAIL",
}
with open(receipt_path, "w") as fh:
    json.dump(receipt, fh, indent=2)

print(f"proofs: {sum(1 for p in proofs if p['result']=='PASS')} PASS / "
      f"{sum(1 for p in proofs if p['result']=='FAIL')} FAIL "
      f"(missing: {missing or 'none'})")
print(f"receipt: {receipt_path}")
if not all_pass:
    sys.exit(1)
PYEOF
echo "KANI proofs .................... PASS"
# Court receipt (same convention as the other drift courts): the run-proofs
# receipt is copied into the run dir as evidence (KANI.RECEIPT.001).
pass "kani-proofs" "phase" "formal-verification"
mkdir -p "$RUN_DIR/courts"
cp "$RECEIPT" "$RUN_DIR/courts/kani-proofs.receipt.json" 2>/dev/null || true
