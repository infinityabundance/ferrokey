#!/usr/bin/env bash
# ADAPT.* — the adaptive-geometry courts (Phase 4 WS4, §4.15).
#
# Runs the ADAPT court test inside the builder container (never on the
# host). The test prints one machine-readable gate line per assertion plus a
# JSON metric report; this script parses both, writes the court receipt and
# fails the suite when any gate fails.
#
#   bash testing/scripts/adaptive-court.sh [RUN_ID]
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

LOG="$RUN_DIR/logs/adaptive-court.log"
mkdir -p "$RUN_DIR/logs"

echo "── ADAPT court (adaptive geometry, in the builder container) ──"
run_in_builder bash -c '
    cargo test -p ferrokey-core --test adapt_courts -- --nocapture 2>&1
' | tee "$LOG" || {
    fail "adaptive-geometry" "phase" "adaptive-geometry"
    echo "ADAPT court ...................... FAIL (test runner error)"
    exit 1
}

# Every ADAPT.<ID> line must end in PASS.
GATES=$(grep -E '^ADAPT\.[A-Za-z0-9._]+ ' "$LOG" || true)
FAILED_GATES=$(grep -E '^ADAPT\.[A-Za-z0-9._]+ .*FAIL' "$LOG" || true)
PASS_COUNT=$(printf '%s\n' "$GATES" | grep -c 'PASS$' || true)
FAIL_COUNT=$(printf '%s\n' "$FAILED_GATES" | grep -c 'FAIL' || true)

# The JSON metric report row.
REPORT=$(grep -E '^ADAPT.METRIC.REPORT ' "$LOG" | sed 's/^ADAPT.METRIC.REPORT //' || true)

if [ -n "$FAILED_GATES" ]; then
    printf '%s\n' "$FAILED_GATES"
fi
echo "ADAPT gates: $PASS_COUNT PASS / $FAIL_COUNT FAIL"

if [ "$FAIL_COUNT" -eq 0 ] && [ -n "$REPORT" ]; then
    pass "adaptive-geometry" "phase" "adaptive-geometry" "gates" "$PASS_COUNT"
    # Persist the metric report as evidence.
    printf '%s\n' "$REPORT" > "$RUN_DIR/courts/adaptive-geometry.metrics.json"
    echo "ADAPT court ...................... PASS ($PASS_COUNT gates)"
    exit 0
else
    fail "adaptive-geometry" "phase" "adaptive-geometry" "gates" "$FAIL_COUNT"
    echo "ADAPT court ...................... FAIL"
    exit 1
fi
