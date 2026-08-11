#!/usr/bin/env bash
# Run one VM court: builds the payload, boots a disposable QEMU VM via the
# oracle container, collects evidence, destroys the overlay.
#
#   ./testing/scripts/run-vm-court.sh <court-name> [x11|wayland] [debian-12]
#
# The authoritative VM path (rule 9):
#   host ──docker──▶ oracle container ──qemu──▶ guest kernel
#                                                  └── /dev/uinput + compositor
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

COURT="${1:?usage: run-vm-court.sh <court> [profile] [distro]}"
PROFILE="${2:-x11}"
DISTRO="${3:-debian-12}"

if [ ! -f "$REPO_ROOT/testing/courts/$COURT/court.sh" ]; then
    echo "no court script at testing/courts/$COURT/court.sh"
    exit 1
fi

host_safety_preflight

# ---------------------------------------------------------------------------
# 1. Ensure images exist.
# ---------------------------------------------------------------------------
if ! "$DOCKER" image inspect "$BUILDER_IMAGE" >/dev/null 2>&1 \
    || ! "$DOCKER" image inspect "$TARGETS_IMAGE" >/dev/null 2>&1 \
    || ! "$DOCKER" image inspect "$ORACLE_IMAGE" >/dev/null 2>&1; then
    bash scripts/build-images.sh
fi

# ---------------------------------------------------------------------------
# 2. Build the payload: product binaries + court targets + scripts.
# ---------------------------------------------------------------------------
PAYLOAD_DIR="${PAYLOAD_DIR:-$RUN_DIR/payload}"
rm -rf "$PAYLOAD_DIR"
mkdir -p "$PAYLOAD_DIR"/{bin,courts}
cp -r "$REPO_ROOT/testing/courts/$COURT" "$PAYLOAD_DIR/courts/"
cp -r "$REPO_ROOT/testing/vm/provision" "$PAYLOAD_DIR/" 2>/dev/null || true
cp -r "$REPO_ROOT/testing/fixtures" "$PAYLOAD_DIR/" 2>/dev/null || true

echo "==> building product binaries (builder image)"
"$DOCKER" run --rm \
    -v "$REPO_ROOT:/repo:ro" \
    -v "$PAYLOAD_DIR/bin:/out" \
    -v ferrokey-payload-cargo:/usr/local/cargo \
    -v ferrokey-payload-target:/target \
    -e CARGO_HOME=/usr/local/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e DISPLAY= -e WAYLAND_DISPLAY= -e XDG_RUNTIME_DIR=/tmp \
    -e DBUS_SESSION_BUS_ADDRESS= -e XAUTHORITY= -e SSH_AUTH_SOCK= \
    --network bridge \
    "$BUILDER_IMAGE" \
    bash -c 'cd /repo && cargo build --release -p ferrokey -p ferrokeyd && cp /target/release/ferrokey /target/release/ferrokeyd /out/'

echo "==> building court targets (targets image)"
"$DOCKER" run --rm \
    -v "$REPO_ROOT/testing/targets:/targets:ro" \
    -v "$PAYLOAD_DIR/bin:/out" \
    -v ferrokey-payload-targets:/target \
    -e CARGO_TARGET_DIR=/target \
    -e DISPLAY= -e WAYLAND_DISPLAY= -e XDG_RUNTIME_DIR=/tmp \
    -e DBUS_SESSION_BUS_ADDRESS= -e XAUTHORITY= -e SSH_AUTH_SOCK= \
    --network bridge \
    "$TARGETS_IMAGE" \
    bash -c '
        set -e
        cd /targets
        cargo build --release \
            -p ferrokey-test-common -p ferrokey-test-target-x11 \
            -p ferrokey-test-target-wayland -p ferrokey-test-target-slint \
            -p ferrokey-test-target-gtk -p ferrokey-test-virtinput
        cp /target/release/ferrokey-test-target-x11 \
           /target/release/ferrokey-test-target-wayland \
           /target/release/ferrokey-test-target-slint \
           /target/release/ferrokey-test-target-gtk \
           /target/release/ferrokey-test-virtinput /out/
        # Qt target via CMake
        cmake -S /targets/qt -B /target/qt-build -G Ninja -DCMAKE_BUILD_TYPE=Release >/dev/null
        cmake --build /target/qt-build >/dev/null
        cp /target/qt-build/ferrokey-test-target-qt /out/
    '

echo "payload binaries:"
ls -la "$PAYLOAD_DIR/bin"

# ---------------------------------------------------------------------------
# 3. Run the VM court inside the oracle container (rules 35: narrow device
#    grants only — /dev/kvm is virtualization acceleration, nothing else).
# ---------------------------------------------------------------------------
KVM_ARGS=()
if [ -e "$KVM_DEVICE" ]; then
    KVM_ARGS=(--device "$KVM_DEVICE:$KVM_DEVICE")
fi

"$DOCKER" run --rm \
    "${KVM_ARGS[@]}" \
    -v "$REPO_ROOT:/repo:ro" \
    -v ferrokey-vm-state:/court/state \
    -v "$PAYLOAD_DIR:/court/state/payload:ro" \
    -e COURT="$COURT" \
    -e PROFILE="$PROFILE" \
    -e DISTRO="$DISTRO" \
    --network bridge \
    "$ORACLE_IMAGE" \
    bash /repo/testing/vm/qemu/run-court-inner.sh "$COURT" "$PROFILE"

echo "==> VM court $COURT finished; evidence in $RUN_DIR"
