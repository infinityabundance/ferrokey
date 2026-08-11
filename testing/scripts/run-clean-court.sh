#!/usr/bin/env bash
# The authoritative CLEAN build court (rule 31): empty cargo cache, empty
# target dir, network allowed and declared. Proves the project builds from a
# pristine state.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

echo "==> clean build court (empty caches, network: DECLARED REQUIRED for crates.io) =="
"$DOCKER" volume rm -f ferrokey-clean-cargo ferrokey-clean-target >/dev/null 2>&1 || true
"$DOCKER" run --rm \
    --network bridge \
    -v "$REPO_ROOT:/repo:ro" \
    -v ferrokey-clean-cargo:/usr/local/cargo \
    -v ferrokey-clean-target:/repo/target \
    -e CARGO_HOME=/usr/local/cargo \
    -e CARGO_TARGET_DIR=/repo/target \
    -e DISPLAY= -e WAYLAND_DISPLAY= -e XDG_RUNTIME_DIR=/tmp \
    -e DBUS_SESSION_BUS_ADDRESS= -e XAUTHORITY= -e SSH_AUTH_SOCK= \
    --security-opt no-new-privileges \
    --cap-drop ALL \
    "$BUILDER_IMAGE" \
    bash -c 'cd /repo && cargo build --workspace --all-targets && cargo test --workspace --lib --bins && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check'

if [ $? -eq 0 ]; then
    pass "build.clean" "network" "required-for-cratesio"
    echo "BUILD COURT (clean) .......... PASS"
else
    fail "build.clean"
    echo "BUILD COURT (clean) .......... FAIL"
    exit 1
fi

"$DOCKER" volume rm -f ferrokey-clean-cargo ferrokey-clean-target >/dev/null 2>&1 || true
