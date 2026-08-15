#!/usr/bin/env bash
# The authoritative CLEAN build court (rule 31): empty cargo cache, empty
# target dir, network allowed and declared. Proves the project builds from a
# pristine state.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

echo "==> clean build court (empty caches, network: DECLARED REQUIRED for crates.io) =="
# OOM limits: the clean-build caches are run-scoped bind dirs on the REAL
# disk — fresh per run (identical "empty caches" semantics) and never on the
# tmpfs data-root. Only the registry/git subdirs are mounted (never
# CARGO_HOME): the image's cargo toolchain must stay visible. The container
# also runs under the suite's hard memory cap.
CLEAN_TMP="$RUN_DIR/tmp/clean"
mkdir -p "$CLEAN_TMP"/{registry,git,target}
"$DOCKER" run --rm \
    $(mem_flags) \
    --network "${COURT_NETWORK:-bridge}" \
    -v "$REPO_ROOT:/repo:ro" \
    -v "$CLEAN_TMP/registry:/usr/local/cargo/registry" \
    -v "$CLEAN_TMP/git:/usr/local/cargo/git" \
    -v "$CLEAN_TMP/target:/repo/target" \
    -e CARGO_HOME=/usr/local/cargo \
    -e CARGO_TARGET_DIR=/repo/target \
    -e CARGO_INCREMENTAL=0 \
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
