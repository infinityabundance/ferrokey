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

# The cargo build-cache volumes. OOM limits: the defaults are run-scoped
# bind dirs on the REAL disk (the payload builds are the suite's third
# biggest transient consumer; the tmpfs data-root never holds them).
# Overridable so the mutation runner can isolate its (deliberately mutated)
# builds from the production artifacts: a mutated binary left in the shared
# volume would otherwise be reused by a later court run (cargo fingerprinting
# cannot tell two /repo mounts apart — the mutation suite mounts its
# disposable copy at the same path).
PAYLOAD_TARGET_VOLUME="${PAYLOAD_TARGET_VOLUME:-$RUN_DIR/tmp/payload-target}"
PAYLOAD_TARGETS_VOLUME="${PAYLOAD_TARGETS_VOLUME:-$RUN_DIR/tmp/payload-targets}"
mkdir -p "$PAYLOAD_TARGET_VOLUME" "$PAYLOAD_TARGETS_VOLUME" 2>/dev/null || true

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
# The whole courts tree (shared lib.sh + helpers + every court) is shipped so
# each court script finds its helpers at a stable relative path.
cp -r "$REPO_ROOT/testing/courts/." "$PAYLOAD_DIR/courts/"
cp -r "$REPO_ROOT/testing/vm/provision" "$PAYLOAD_DIR/" 2>/dev/null || true
cp -r "$REPO_ROOT/testing/fixtures" "$PAYLOAD_DIR/" 2>/dev/null || true
# The systemd court installs the hardened unit from the packaging tree (§38).
cp -r "$REPO_ROOT/PACKAGING" "$PAYLOAD_DIR/" 2>/dev/null || true
# The Electron court app (a script-only app; nothing to compile).
cp -r "$REPO_ROOT/testing/targets/electron" "$PAYLOAD_DIR/electron" 2>/dev/null || true

echo "==> building product binaries (builder image)"
# The payload-cargo volume overlays only the registry: a volume mounted over
# /usr/local/cargo would hide the rust image's cargo toolchain.
"$DOCKER" run --rm \
    $(mem_flags) \
    --network "${COURT_NETWORK:-bridge}" \
    -v "$REPO_ROOT:/repo:ro" \
    -v "$PAYLOAD_DIR/bin:/out" \
    -v ferrokey-payload-cargo:/usr/local/cargo/registry \
    -v "$PAYLOAD_TARGET_VOLUME:/target" \
    -e CARGO_HOME=/usr/local/cargo \
    -e CARGO_TARGET_DIR=/target \
    -e CARGO_INCREMENTAL=0 \
    -e DISPLAY= -e WAYLAND_DISPLAY= -e XDG_RUNTIME_DIR=/tmp \
    -e DBUS_SESSION_BUS_ADDRESS= -e XAUTHORITY= -e SSH_AUTH_SOCK= \
    "$BUILDER_IMAGE" \
    bash -c 'cd /repo && cargo build --release -p ferrokey -p ferrokeyd && cp /target/release/ferrokey /target/release/ferrokeyd /out/'

echo "==> building court targets (targets image)"
"$DOCKER" run --rm \
    $(mem_flags) \
    --network "${COURT_NETWORK:-bridge}" \
    -v "$REPO_ROOT/testing/targets:/targets:ro" \
    -v "$PAYLOAD_DIR/bin:/out" \
    -v "$PAYLOAD_TARGETS_VOLUME:/target" \
    -e CARGO_TARGET_DIR=/target \
    -e CARGO_INCREMENTAL=0 \
    -e DISPLAY= -e WAYLAND_DISPLAY= -e XDG_RUNTIME_DIR=/tmp \
    -e DBUS_SESSION_BUS_ADDRESS= -e XAUTHORITY= -e SSH_AUTH_SOCK= \
    "$TARGETS_IMAGE" \
    bash -c '
        set -e
        # The workspace mount is read-only; copy to a writable location so
        # Cargo can write its lockfile.
        rm -rf /work && cp -a /targets /work
        cd /work
        cargo build --release \
            -p ferrokey-test-common -p ferrokey-test-target-x11 \
            -p ferrokey-test-target-wayland -p ferrokey-test-target-slint \
            -p ferrokey-test-target-gtk -p ferrokey-test-target-sdl \
            -p ferrokey-test-layer-probe -p ferrokey-test-virtinput \
            -p ferrokey-test-mini-compositor
        cp /target/release/ferrokey-test-target-x11 \
           /target/release/ferrokey-test-target-wayland \
           /target/release/ferrokey-test-target-slint \
           /target/release/ferrokey-test-target-gtk \
           /target/release/ferrokey-test-target-sdl \
           /target/release/ferrokey-test-layer-probe \
           /target/release/ferrokey-test-virtinput \
           /target/release/ferrokey-test-mini-compositor /out/
        # Qt target via CMake
        cmake -S /targets/qt -B /target/qt-build -G Ninja -DCMAKE_BUILD_TYPE=Release >/dev/null
        cmake --build /target/qt-build >/dev/null
        cp /target/qt-build/ferrokey-test-target-qt /out/
        # The fake-touch court helper: a uinput touchscreen for the touch
        # court (guest-only; never installed on the host).
        gcc -O2 -Wall -o /out/fake-touch /targets/fake-touch.c
        # The pty-oracle probe: the deterministic PTY child for the embedded
        # terminal-workspace courts (reports bytes/winsize/signals, §99).
        gcc -O2 -Wall -o /out/pty-oracle /targets/pty-oracle.c
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
    $(mem_flags) \
    --network "${COURT_NETWORK:-bridge}" \
    -v "$REPO_ROOT:/repo:ro" \
    -v ferrokey-vm-state:/court/state \
    -v "$PAYLOAD_DIR:/court/state/payload:ro" \
    -v ferrokey-kasan-kernel:/kasan-kernel:ro \
    -e COURT="$COURT" \
    -e PROFILE="$PROFILE" \
    -e DISTRO="$DISTRO" \
    -e MUTATION="${MUTATION:-}" \
    -e SOAK_SECONDS="${SOAK_SECONDS:-}" \
    -e KASAN="${KASAN:-}" \
    "$ORACLE_IMAGE" \
    /repo/testing/vm/qemu/run-court-inner.sh "$COURT" "$PROFILE"

echo "==> VM court $COURT finished; evidence in $RUN_DIR"
