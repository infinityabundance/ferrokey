#!/usr/bin/env bash
# BUILD + CORE unit courts in the Docker builder (rules 31/32/33/34).
#
#   ./testing/scripts/run-unit-court.sh
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env
host_safety_preflight

# Pre-warm the cargo cache from the host's downloaded crates (pure
# performance optimization; the clean court proves cache-independence).
"$DOCKER" volume create ferrokey-cargo-cache >/dev/null 2>&1 || true
"$DOCKER" volume create ferrokey-target-cache >/dev/null 2>&1 || true

# Cargo cache pre-warm: copy the host registry (downloads only — no test
# runs on the host, nothing touches input devices or the desktop).
if [ -d "${HOME}/.cargo/registry" ]; then
    "$DOCKER" run --rm \
        -v ferrokey-cargo-cache:/cache \
        -v "${HOME}/.cargo/registry:/src/registry:ro" \
        alpine sh -c 'cp -a /src/registry/. /cache/ 2>/dev/null || true' >/dev/null 2>&1 || true
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
