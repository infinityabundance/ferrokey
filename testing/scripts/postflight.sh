#!/usr/bin/env bash
# Host-safety postflight (rule 44). Proves the court run left no trace on
# the host: no devices, no daemon, no udev changes, no tree changes outside
# the evidence dir.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

host_safety_postflight

# The working tree must be unchanged except evidence (rule 32).
porcelain="$(git -C "$REPO_ROOT" status --porcelain 2>/dev/null | grep -v '^?? testing/evidence/' || true)"
if [ -n "$porcelain" ]; then
    echo "POSTFLIGHT FAIL: working tree changed:"
    echo "$porcelain"
    exit 1
fi
echo "WORKING TREE ................. CLEAN (except evidence)"
