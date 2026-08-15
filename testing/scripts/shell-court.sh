#!/usr/bin/env bash
# SHELL.* — the shell-aware terminal rows courts (Phase 4 WS5, §5.15).
#
# Runs the SHELL court test inside the builder container (never on the
# host). The test prints one machine-readable gate line per assertion; this
# script parses them, writes the court receipt and fails the suite when any
# gate fails.
#
#   bash testing/scripts/shell-court.sh [RUN_ID]
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

LOG="$RUN_DIR/logs/shell-court.log"
mkdir -p "$RUN_DIR/logs"

echo "── SHELL court (shell-aware rows, in the builder container) ──"
run_in_builder bash -c '
    cargo test -p ferrokey-terminal --test shell_courts -- --nocapture 2>&1
' | tee "$LOG" || {
    fail "shell-rows" "phase" "shell-aware-rows"
    echo "SHELL court ...................... FAIL (test runner error)"
    exit 1
}

GATES=$(grep -E '^SHELL\.[A-Za-z0-9._]+ ' "$LOG" || true)
FAILED_GATES=$(grep -E '^SHELL\.[A-Za-z0-9._]+ .*FAIL' "$LOG" || true)
PASS_COUNT=$(printf '%s\n' "$GATES" | grep -c 'PASS$' || true)
FAIL_COUNT=$(printf '%s\n' "$FAILED_GATES" | grep -c 'FAIL' || true)

if [ -n "$FAILED_GATES" ]; then
    printf '%s\n' "$FAILED_GATES"
fi
echo "SHELL gates: $PASS_COUNT PASS / $FAIL_COUNT FAIL"

if [ "$FAIL_COUNT" -eq 0 ] && [ "$PASS_COUNT" -gt 0 ]; then
    pass "shell-rows" "phase" "shell-aware-rows" "gates" "$PASS_COUNT"
    echo "SHELL court ...................... PASS ($PASS_COUNT gates)"
    exit 0
else
    fail "shell-rows" "phase" "shell-aware-rows" "gates" "$FAIL_COUNT"
    echo "SHELL court ...................... FAIL"
    exit 1
fi
