#!/usr/bin/env bash
# KANI.MUTATION.001 — proof negative controls (Phase 4 WS3, §3.9).
#
# For each major proof family, a controlled regression is introduced into a
# disposable COPY of the production state machine, and the corresponding
# harness MUST FAIL on the mutated code. A mutation that the harness fails to
# detect (SUCCESSFUL verification) is a failed negative control — the proof
# would not catch the regression.
#
# Mutations (each coherent with a real-world regression):
#
#   MUT.ROLLOVER   remove the rollover guard in press()
#                  → KANI.ROLLOVER.001 must FAIL
#   MUT.DUPLICATE  allow duplicate held insertion (press guard + set guard)
#                  → KANI.HELD.001 must FAIL
#   MUT.LATCH      latch is never consumed by a qualifying press
#                  → KANI.LATCH.001 must FAIL
#   MUT.RELEASEALL release_all omits one held key's Up event
#                  → KANI.RELEASEALL.001 must FAIL
#
# Like run-proofs.sh, everything runs inside the ferrokey-kani container
# (never on the host) with the same OOM guardrails.
#
#   bash proofs/run-negative-controls.sh
set -euo pipefail
cd "$(dirname "$0")/.."
source testing/scripts/lib.sh
sanitize_env

RECEIPT="$REPO_ROOT/proofs/kani-mutation-receipt.json"
COMMIT=$(git -C "$REPO_ROOT" rev-parse HEAD)
KANI_VERSION=$("$DOCKER" run --rm "$KANI_IMAGE" kani --version 2>/dev/null | awk '{print $2}' || echo unknown)

KANI_MEM_LIMIT="${KANI_MEM_LIMIT:-48g}"

if ! "$DOCKER" image inspect "$KANI_IMAGE" >/dev/null 2>&1; then
    bash testing/scripts/build-images.sh
fi

echo "── running Kani negative controls (ferrokey-kani container) ──"

"$DOCKER" run --rm \
    --memory "$KANI_MEM_LIMIT" \
    --memory-swap "$KANI_MEM_LIMIT" \
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
        sed -i "s/^rust-version = \"1.96\"/rust-version = \"1.93\"/" /work/src/Cargo.toml
        cd /work/src

        # mutation spec: name | harness | sed script (applied to the copy)
        declare -A MUT
        declare -A MUT_SED_
        MUT[MUT.ROLLOVER]=kani_rollover_held_bound
        MUT_SED_[MUT.ROLLOVER]="s/if self.depressed.len() >= self.settings.max_held_keys {/if false {/"
        MUT[MUT.DUPLICATE]=kani_held_unique
        MUT_SED_[MUT.DUPLICATE]="s/if self.depressed.contains(key) {/if false {/"
        MUT[MUT.LATCH]=kani_latch_semantics
        MUT_SED_[MUT.LATCH]="/The latch is consumed by the first key pressed after it./,+1s/self.latched = ModifierSet::empty();/\/\/ latch consumption removed (mutation)/"
        MUT[MUT.RELEASEALL]=kani_release_all_complete
        MUT_SED_[MUT.RELEASEALL]="s/if idx < n \&\& !keys\[idx\].is_modifier() {/if idx < n \&\& idx != 0 \&\& !keys[idx].is_modifier() {/"

        results_file=/work/mutation-results.txt
        : > "$results_file"
        for name in MUT.ROLLOVER MUT.DUPLICATE MUT.LATCH MUT.RELEASEALL; do
            harness=${MUT[$name]}
            sed_script=${MUT_SED_[$name]}
            echo "── $name ($harness) ──"
            cp -r /work/src "/work/mut-$name"
            # apply the mutation to the disposable copy
            case "$name" in
                MUT.DUPLICATE)
                    sed -i "$sed_script" "/work/mut-$name/crates/ferrokey-core/src/state.rs"
                    sed -i "s/if self.len >= MAX_HELD_KEYS || self.contains(key) {/if self.len >= MAX_HELD_KEYS {/" "/work/mut-$name/crates/ferrokey-core/src/keyset.rs"
                    ;;
                *)
                    sed -i "$sed_script" "/work/mut-$name/crates/ferrokey-core/src/state.rs"
                    ;;
            esac
            ( cd "/work/mut-$name" && \
              cargo kani -p ferrokey-proofs --harness "$harness" \
                -Z unstable-options --harness-timeout 20m \
                --cbmc-args --unwind 33 --unwinding-assertions \
                2>&1 | grep -E "VERIFICATION:- (SUCCESSFUL|FAILED)" | sed "s/^/$name: /" \
              ) >> "$results_file" || true
            echo "$name done"
        done

        python3 - "$results_file" <<"PYEOF"
import sys
results_file = sys.argv[1]
EXPECT = {
    "MUT.ROLLOVER": "kani_rollover_held_bound",
    "MUT.DUPLICATE": "kani_held_unique",
    "MUT.LATCH": "kani_latch_semantics",
    "MUT.RELEASEALL": "kani_release_all_complete",
}
ok = True
seen = {}
for line in open(results_file):
    if ": VERIFICATION:- " in line:
        name, verdict = line.strip().split(": VERIFICATION:- ")
        seen[name] = verdict
for name, harness in EXPECT.items():
    verdict = seen.get(name, "MISSING")
    detected = verdict == "FAILED"
    print(f"{name:14s} harness {harness:26s} verdict {verdict:12s} "
          + ("DETECTED (good)" if detected else "NOT DETECTED (bad)"))
    ok = ok and detected
sys.exit(0 if ok else 1)
PYEOF
    ' 2>&1 | tee /tmp/kani-mutation.log
MUT_OK=${PIPESTATUS[0]}

# Machine-readable receipt (KANI.MUTATION.001).
cat > "$RECEIPT" <<EOF
{
  "tool": "kani",
  "tool_version": "$KANI_VERSION",
  "commit": "$COMMIT",
  "court": "KANI.MUTATION.001",
  "result": "$( [ "$MUT_OK" -eq 0 ] && echo PASS || echo FAIL )",
  "mutations": [
    {"id": "MUT.ROLLOVER", "regression": "rollover guard removed", "expect": "KANI.ROLLOVER.001 FAIL"},
    {"id": "MUT.DUPLICATE", "regression": "duplicate held insertion allowed", "expect": "KANI.HELD.001 FAIL"},
    {"id": "MUT.LATCH", "regression": "latch consumption removed", "expect": "KANI.LATCH.001 FAIL"},
    {"id": "MUT.RELEASEALL", "regression": "release_all omits one Up", "expect": "KANI.RELEASEALL.001 FAIL"}
  ]
}
EOF

if [ "$MUT_OK" -eq 0 ]; then
    echo "KANI negative controls ........ PASS (every mutation detected)"
    # Court receipt: same convention as the other courts.
    pass "kani-mutations" "phase" "formal-verification"
    mkdir -p "$RUN_DIR/courts"
    cp "$RECEIPT" "$RUN_DIR/courts/kani-mutations.receipt.json" 2>/dev/null || true
else
    echo "KANI negative controls ........ FAIL (a mutation was not detected)"
    fail "kani-mutations" "phase" "formal-verification"
    exit 1
fi
