#!/usr/bin/env bash
# SC.SUPPLY.* — the supply-chain gate (Phase 5).
#
# Runs `cargo deny check` (advisories, licenses, bans, sources) inside the
# disposable Docker builder — never on the host — against the committed
# deny.toml + Cargo.lock. The policy (deny.toml, reviewed in
# docs/supply-chain.md) is enforced mechanically: every matching advisory,
# unknown/unlicensed license, wildcard version, or non-crates.io source
# fails the court, and so does stale policy (unused-ignored-advisory,
# unused-allowed-license, unused-license-exception — drift controls that
# make a rotting allow/ignore list fail instead of silently passing).
#
# Negative control: a probe policy containing a never-valid advisory ID is
# run through the same gate; the gate MUST reject it. If it passes, the gate
# is broken and the court fails.
#
#   bash testing/scripts/supply-chain-court.sh [RUN_ID]
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

LOG="$RUN_DIR/logs/supply-chain-court.log"
NEG_LOG="$RUN_DIR/logs/supply-chain-negative.log"
mkdir -p "$RUN_DIR/logs" "$RUN_DIR/tmp"

echo "── SUPPLY-CHAIN court (cargo-deny 0.20.2 in the builder container) ──"
# pipefail propagates cargo-deny's exit status through tee; the `if` form
# keeps the suite's `set -e` from firing before the receipt is written.
if run_in_builder cargo deny check 2>&1 | tee "$LOG"; then
    :
else
    fail "supply-chain" "phase" "supply-chain-gate" "tool" "cargo-deny-0.20.2"
    echo "SUPPLY-CHAIN court .............. FAIL (cargo-deny exit non-zero)"
    exit 1
fi

echo "── SUPPLY-CHAIN negative control (bogus advisory ID must fail) ──"
# Probe policy = the real deny.toml + a never-valid advisory ID injected into
# the ignore list. The gate must reject it (unused-ignored-advisory = "deny");
# a probe that passes means the gate cannot fail, which fails the court.
PROBE="$RUN_DIR/tmp/deny-probe.toml"
awk 'BEGIN{done=0} { if ($0 == "]" && !done) { print "    { id = \"RUSTSEC-0000-0000\", reason = \"negative control probe: must fail\" },"; done=1 } print }' \
    "$REPO_ROOT/deny.toml" > "$PROBE"
grep -q 'RUSTSEC-0000-0000' "$PROBE" || { echo "SUPPLY-CHAIN negative control ... FAIL (probe not built)"; exit 1; }

make_bind_writable "$RUN_DIR/tmp/workspace-registry" "$RUN_DIR/tmp/workspace-target"
set +e
"$DOCKER" run --rm \
    $(mem_flags) \
    --network "${COURT_NETWORK:-bridge}" \
    -v "$REPO_ROOT:/repo:ro" \
    -v "$PROBE:/repo/deny.toml:ro" \
    -v "$RUN_DIR/tmp/workspace-registry:/usr/local/cargo/registry" \
    -v "$RUN_DIR/tmp/workspace-target:/repo/target" \
    -e CARGO_HOME=/usr/local/cargo -e CARGO_TARGET_DIR=/repo/target \
    -e DISPLAY= -e WAYLAND_DISPLAY= -e XDG_RUNTIME_DIR=/tmp \
    -e DBUS_SESSION_BUS_ADDRESS= -e XAUTHORITY= -e SSH_AUTH_SOCK= \
    --security-opt no-new-privileges --cap-drop ALL \
    "$BUILDER_IMAGE" cargo deny check > "$NEG_LOG" 2>&1
NEG_RC=$?
set -e

if [ "$NEG_RC" -eq 0 ]; then
    fail "supply-chain" "phase" "supply-chain-gate" "tool" "cargo-deny-0.20.2" "negative-control" "FAIL"
    echo "SUPPLY-CHAIN negative control ... FAIL (gate accepted a bogus advisory ID)"
    exit 1
fi

pass "supply-chain" "phase" "supply-chain-gate" "tool" "cargo-deny-0.20.2" "negative-control" "PASS"
echo "SUPPLY-CHAIN negative control ... PASS (gate rejected the probe)"
echo "SUPPLY-CHAIN court .............. PASS"
