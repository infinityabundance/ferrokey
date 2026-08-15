#!/usr/bin/env bash
# Shared helpers for the Ferrokey compatibility courts.
#
# The host is an ORCHESTRATOR ONLY. Nothing in this file (or any court
# script) may open /dev/uinput, create input devices, touch host udev, or
# connect to a host GUI session.

set -u

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TESTING_DIR="$REPO_ROOT/testing"
EVIDENCE_DIR="$TESTING_DIR/evidence"
SCRIPTS_DIR="$TESTING_DIR/scripts"
RUN_ID="${RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
RUN_DIR="$EVIDENCE_DIR/$RUN_ID"
DOCKER="${DOCKER:-docker}"
KVM_DEVICE="${KVM_DEVICE:-/dev/kvm}"
BUILDER_IMAGE="${BUILDER_IMAGE:-ferrokey-builder:latest}"
ORACLE_IMAGE="${ORACLE_IMAGE:-ferrokey-oracle:latest}"
TARGETS_IMAGE="${TARGETS_IMAGE:-ferrokey-targets:latest}"
KANI_IMAGE="${KANI_IMAGE:-ferrokey-kani:latest}"

mkdir -p "$RUN_DIR"/{courts,logs,devices,screenshots}

# ---------------------------------------------------------------------------
# Environment sanitization (rule 33): the court must never see the host GUI.
# ---------------------------------------------------------------------------
sanitize_env() {
    export DISPLAY="" \
        WAYLAND_DISPLAY="" \
        XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR_COURT:-/tmp/court-runtime}" \
        DBUS_SESSION_BUS_ADDRESS="" \
        XAUTHORITY="" \
        SSH_AUTH_SOCK="" \
        GIT_DIR="$REPO_ROOT/.git"
    mkdir -p "$XDG_RUNTIME_DIR"
}

# ---------------------------------------------------------------------------
# Receipts (rule 38): structured, machine-readable results.
# ---------------------------------------------------------------------------
write_receipt() {
    local court="$1" result="$2"
    shift 2
    # [key value ...] pairs become JSON members. Each pair is two args;
    # anything else would produce a stray string literal and break the JSON
    # (the receipt parser then silently drops the whole receipt — §37).
    local extra=""
    while [ "$#" -ge 2 ]; do
        extra="$extra, \"$1\": \"$2\""
        shift 2
    done
    mkdir -p "$RUN_DIR/courts"
    cat > "$RUN_DIR/courts/$court.receipt.json" <<EOF
{
  "court": "$court",
  "result": "$result",
  "ferrokey_commit": "$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)",
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "run_id": "$RUN_ID"
  $extra
}
EOF
    echo "$result" > "$RUN_DIR/courts/$court.result"
}

pass() { write_receipt "$1" "PASS" "${@:2}"; }
fail() { write_receipt "$1" "FAIL" "${@:2}"; }

# ---------------------------------------------------------------------------
# Host-safety preflight (rule 43): abort (not warn) on any violation.
# ---------------------------------------------------------------------------
host_safety_preflight() {
    local problems=0

    # 1. No Ferrokey test may ever see the host input subsystem.
    if [ -n "${DISPLAY:-}" ]; then problems=$((problems+1)); echo "PREFLIGHT FAIL: host DISPLAY is set"; fi
    if [ -n "${WAYLAND_DISPLAY:-}" ]; then problems=$((problems+1)); echo "PREFLIGHT FAIL: host WAYLAND_DISPLAY is set"; fi
    if [ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ]; then problems=$((problems+1)); echo "PREFLIGHT FAIL: host DBUS_SESSION_BUS_ADDRESS is set"; fi

    # 2. No host ferrokeyd process.
    if pgrep -x ferrokeyd >/dev/null 2>&1; then
        problems=$((problems+1)); echo "PREFLIGHT FAIL: a ferrokeyd process is running on the host"
    fi

    # 3. No Ferrokey virtual device on the host.
    if grep -q "Ferrokey" /proc/bus/input/devices 2>/dev/null; then
        problems=$((problems+1)); echo "PREFLIGHT FAIL: a Ferrokey virtual device exists on the host"
    fi

    # 4. No host udev rules touched by Ferrokey.
    if grep -rl "ferrokey" /etc/udev/rules.d/ 2>/dev/null | grep -q .; then
        problems=$((problems+1)); echo "PREFLIGHT FAIL: host udev rules reference ferrokey"
    fi

    # 5. The docker binary must be available.
    if ! command -v "$DOCKER" >/dev/null 2>&1; then
        problems=$((problems+1)); echo "PREFLIGHT FAIL: docker not found"
    fi

    if [ "$problems" -gt 0 ]; then
        echo "HOST SAFETY PREFLIGHT ........ FAIL"
        exit 1
    fi
    echo "HOST SAFETY PREFLIGHT ........ PASS"
}

# ---------------------------------------------------------------------------
# Host-safety postflight (rule 44): prove the court did not contaminate.
# ---------------------------------------------------------------------------
host_safety_postflight() {
    local problems=0
    if grep -q "Ferrokey" /proc/bus/input/devices 2>/dev/null; then
        problems=$((problems+1)); echo "POSTFLIGHT FAIL: Ferrokey device found on host"
    fi
    if pgrep -x ferrokeyd >/dev/null 2>&1; then
        problems=$((problems+1)); echo "POSTFLIGHT FAIL: ferrokeyd running on host"
    fi
    if grep -rl "ferrokey" /etc/udev/rules.d/ 2>/dev/null | grep -q .; then
        problems=$((problems+1)); echo "POSTFLIGHT FAIL: host udev rules reference ferrokey"
    fi
    # The verdict is recorded for the security seal (§90: host contamination).
    mkdir -p "$RUN_DIR"
    if [ "$problems" -gt 0 ]; then
        echo "HOST CONTAMINATION ........... DETECTED"
        echo "HOST SAFETY POSTFLIGHT ....... FAIL"
        echo "DETECTED" > "$RUN_DIR/host-contamination.txt"
        exit 1
    fi
    echo "HOST CONTAMINATION ........... NONE"
    echo "HOST SAFETY POSTFLIGHT ....... PASS"
    echo "NONE" > "$RUN_DIR/host-contamination.txt"
}

# ---------------------------------------------------------------------------
# Docker court runner: run a command in the builder image with a read-only
# repo mount and container-owned caches (rules 31/32/33/34).
# ---------------------------------------------------------------------------
run_in_builder() {
    local cache_volume="${CARGO_CACHE_VOLUME:-ferrokey-cargo-cache}"
    local target_volume="${TARGET_CACHE_VOLUME:-ferrokey-target-cache}"
    # The cache volume overlays only the registry: a volume mounted over
    # /usr/local/cargo would hide the rust image's cargo toolchain.
    "$DOCKER" run --rm \
        --network "${COURT_NETWORK:-bridge}" \
        -v "$REPO_ROOT:/repo:ro" \
        -v "$cache_volume:/usr/local/cargo/registry" \
        -v "$target_volume:/repo/target" \
        -e CARGO_HOME=/usr/local/cargo \
        -e CARGO_TARGET_DIR=/repo/target \
        -e DISPLAY= -e WAYLAND_DISPLAY= -e XDG_RUNTIME_DIR=/tmp \
        -e DBUS_SESSION_BUS_ADDRESS= -e XAUTHORITY= -e SSH_AUTH_SOCK= \
        --security-opt no-new-privileges \
        --cap-drop ALL \
        "$BUILDER_IMAGE" "$@"
}

# ---------------------------------------------------------------------------
# Evidence: hash an artifact into the run dir.
# ---------------------------------------------------------------------------
hash_artifact() {
    local src="$1" name="$2"
    if [ -f "$src" ]; then
        sha256sum "$src" > "$RUN_DIR/$name.sha256"
    fi
}

collect_device_evidence() {
    # Called from inside a VM: capture kernel input-device evidence.
    {
        echo "=== /proc/bus/input/devices ==="
        cat /proc/bus/input/devices 2>/dev/null
        echo "=== /sys/class/input ==="
        ls -la /sys/class/input 2>/dev/null
        echo "=== /dev/input ==="
        ls -la /dev/input 2>/dev/null
    } > /tmp/evidence-devices.txt
}
