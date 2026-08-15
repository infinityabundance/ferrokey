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

mkdir -p "$RUN_DIR"/{courts,logs,devices,screenshots,tmp}

# ---------------------------------------------------------------------------
# OOM limits — the bounded-resource layer.
#
# The docker data-root lives on a 26G tmpfs at /run, so the suite must keep
# the tmpfs inside its limit AND must never let a container drag the host
# toward its own OOM killer. Two mechanisms, applied to every heavy stage:
#
#   memory  every court container runs under a hard cap with swap disabled
#           (--memory == --memory-swap): a runaway build/proof/VM is OOM-
#           killed INSIDE the container and its stage fails loudly, while the
#           host keeps running. 48g is the proven cap for the heaviest
#           container in the suite (the Kani verifier); everything else peaks
#           well below it. Override with COURT_MEM_LIMIT.
#
#   disk    the three largest transient consumers (workspace target+registry,
#           the clean-build caches, the VM payload build targets) live in
#           run-scoped bind dirs under $RUN_DIR/tmp on the REAL disk — never
#           the tmpfs data-root. require_headroom() gates every heavy stage on
#           data-root headroom so a shortfall aborts the suite BEFORE a stage
#           instead of ENOSPC-corrupting it mid-build.
# ---------------------------------------------------------------------------
COURT_MEM_LIMIT="${COURT_MEM_LIMIT:-48g}"

mem_flags() {
    # --memory-swap == --memory disables swap at the cap: the container OOMs
    # at the limit (its stage dies with a clear error) instead of pushing the
    # host toward OOM. Intentionally unquoted: word-splits into docker args.
    printf '%s' "--memory $COURT_MEM_LIMIT --memory-swap $COURT_MEM_LIMIT"
}

data_root_headroom_gib() {
    local root have_kib
    root=$("$DOCKER" info --format '{{.DockerRootDir}}' 2>/dev/null || echo /var/lib/docker)
    # df -Pk: 1K blocks; convert KiB -> GiB (integer floor). -Pg is not
    # portable across df implementations.
    have_kib=$(df -Pk "$root" 2>/dev/null | awk 'NR==2 {print $4}')
    [ -n "$have_kib" ] && echo $(( have_kib / 1024 / 1024 ))
}

require_headroom() {
    # require_headroom <stage> <min-gib> — abort before a stage can fill the
    # data-root. A missing measurement only warns (some hosts restrict df).
    local stage="$1" min="${2:-6}" have
    have=$(data_root_headroom_gib)
    if [ -z "$have" ]; then
        echo "WARNING: '$stage' — cannot measure docker data-root headroom; continuing"
        return 0
    fi
    if [ "$have" -lt "$min" ]; then
        echo "ERROR: '$stage' needs >= ${min} GiB free on the docker data-root"
        echo "       (only ${have} GiB free). Free space (docker system prune,"
        echo "       docker volume rm of court caches) and re-run."
        echo "       Refusing to start: an ENOSPC mid-build would corrupt the run."
        exit 1
    fi
    echo "'$stage': ${have} GiB free on the docker data-root (needs >= ${min} GiB)"
}

drop_image()   { "$DOCKER" rmi "$1" >/dev/null 2>&1 || true; }
drop_volume()  { "$DOCKER" volume rm -f "$1" >/dev/null 2>&1 || true; }

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
    # OOM limits: the workspace target + registry are the suite's largest
    # transient consumers; they live in run-scoped bind dirs on the REAL disk
    # (never the tmpfs data-root), and the container runs under a hard memory
    # cap. The registry cache still overlays only the registry subdir: a
    # volume mounted over /usr/local/cargo would hide the rust image's cargo
    # toolchain.
    local cache_dir="${CARGO_CACHE_DIR:-$RUN_DIR/tmp/workspace-registry}"
    local target_dir="${TARGET_CACHE_DIR:-$RUN_DIR/tmp/workspace-target}"
    mkdir -p "$cache_dir" "$target_dir"
    "$DOCKER" run --rm \
        $(mem_flags) \
        --network "${COURT_NETWORK:-bridge}" \
        -v "$REPO_ROOT:/repo:ro" \
        -v "$cache_dir:/usr/local/cargo/registry" \
        -v "$target_dir:/repo/target" \
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
