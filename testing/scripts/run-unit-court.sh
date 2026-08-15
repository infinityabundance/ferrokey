#!/usr/bin/env bash
# BUILD + CORE unit courts in the Docker builder (rules 31/32/33/34).
#
#   ./testing/scripts/run-unit-court.sh
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env
host_safety_preflight

# Pre-warm the run-scoped registry cache from the host's downloaded crates
# (pure performance optimization; the clean court proves cache-independence).
# The cache is a bind dir on the REAL disk (OOM limits: the tmpfs data-root
# never holds it). The copy runs inside a container: the host is an
# orchestrator only. The copies are re-chmodded: the host registry contains
# owner-only (0640) files that a different container uid could not read.
REGISTRY_DIR="$RUN_DIR/tmp/workspace-registry"
mkdir -p "$REGISTRY_DIR"
if [ -d "${HOME}/.cargo/registry" ]; then
    "$DOCKER" run --rm \
        -v "$REGISTRY_DIR:/cache" \
        -v "${HOME}/.cargo/registry:/src/registry:ro" \
        alpine sh -c 'cp -a /src/registry/. /cache/ 2>/dev/null || true; find /cache -type d -exec chmod 755 {} + 2>/dev/null; find /cache -type f -exec chmod 644 {} + 2>/dev/null' >/dev/null 2>&1 || true
fi

if ! run_in_builder bash /repo/testing/courts/build/court.sh; then
    fail "build.workspace" "phase" "build+test+clippy+fmt"
    echo "BUILD COURT ................... FAIL"
    echo "CORE COURT ..................... FAIL"
    exit 1
fi

pass "build.workspace" "phase" "build+test+clippy+fmt"
pass "core.unit" "phase" "cargo test workspace"
echo "BUILD COURT ................... PASS"
echo "CORE COURT .................... PASS"
