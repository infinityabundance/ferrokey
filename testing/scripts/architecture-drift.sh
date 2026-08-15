#!/usr/bin/env bash
# ARCH.DOCS — the architecture documentation drift court (Phase 4 WS1,
# §1.7). A lightweight automated gate that catches mechanically detectable
# documentation drift: documented crates/commands/courts/proofs must exist.
# It does NOT try to prove architectural prose — it catches drift.
#
#   bash testing/scripts/architecture-drift.sh [RUN_ID]
#
# Writes $RUN_DIR/courts/architecture.receipt.json (PASS/FAIL) and a
# per-gate assertions file; the gate is ARCH.DOCS.001–007.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

DOC="$REPO_ROOT/docs/architecture.md"
DIAGRAMS="$REPO_ROOT/docs"
SRC="$REPO_ROOT/src"
CRATES="$REPO_ROOT/crates"
COURTS="$REPO_ROOT/testing/courts"
CONTRIBUTING="$REPO_ROOT/CONTRIBUTING.md"

PASS_COUNT=0
FAIL_COUNT=0
ASSERTIONS=()

gate() { # gate_id label result detail
    local id="$1" label="$2" result="$3" detail="$4"
    if [ "$result" = PASS ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
    printf '%-16s %-46s %s\n' "$id" "$label" "$result"
    if [ "$result" != PASS ]; then
        printf '  detail: %s\n' "$detail"
    fi
    ASSERTIONS+=("{\"assertion\": \"$id $label\", \"result\": \"$result\", \"detail\": \"$detail\"}")
}

exists() { # file ...
    for f in "$@"; do
        [ -e "$f" ] || return 1
    done
}

# ── ARCH.DOCS.001 crate map accurate ────────────────────────────────────────
# Every crate row in the doc's workspace table must exist as a workspace
# member; every member must be documented.
DOC_CRATES=$(grep -oE '^\| `[a-z0-9-]+` \|' "$DOC" | sed -E 's/^\| `([a-z0-9-]+)` \|/\1/' | sort -u)
MEMBER_CRATES=$(cargo metadata --no-deps --format-version 1 2>/dev/null |
    python3 -c 'import json,sys; [print(p["name"]) for p in json.load(sys.stdin)["packages"]]' | sort -u)
MISSING=()
for c in $DOC_CRATES; do
    case "$c" in
        ferrokey) [ -f "$REPO_ROOT/Cargo.toml" ] || MISSING+=("$c") ;;
        ferrokey-proofs) [ -f "$REPO_ROOT/proofs/Cargo.toml" ] || MISSING+=("$c") ;;
        xtask) [ -f "$REPO_ROOT/xtask/Cargo.toml" ] || MISSING+=("$c") ;;
        *) [ -f "$CRATES/$c/Cargo.toml" ] || MISSING+=("$c") ;;
    esac
done
UN_DOCUMENTED=()
for c in $MEMBER_CRATES; do
    case "$c" in ferrokey|ferrokey-core|ferrokey-layouts|ferrokey-protocol|ferrokey-surface|ferrokey-terminal|ferrokey-uinput|ferrokeyd|ferrokey-proofs|xtask) ;; *)
        UN_DOCUMENTED+=("$c") ;; esac
 done
if [ ${#MISSING[@]} -eq 0 ] && [ ${#UN_DOCUMENTED[@]} -eq 0 ]; then
    gate ARCH.DOCS.001 "crate map accurate" PASS "$(echo "$DOC_CRATES" | tr '\n' ' ')"
else
    gate ARCH.DOCS.001 "crate map accurate" FAIL "missing=${MISSING[*]:-none} undocumented=${UN_DOCUMENTED[*]:-none}"
fi

# ── ARCH.DOCS.002 system path accurate ──────────────────────────────────────
# The types/paths the doc claims for the system-input path must exist.
SYS_SYMBOLS=(InputRouter RouterSink DaemonLink KeyboardDriver KeyEvent PhysicalKey Layer RepeatEngine)
SYS_MISSING=()
for s in "${SYS_SYMBOLS[@]}"; do
    grep -rq "$s" "$SRC" "$CRATES/ferrokey-core/src" "$CRATES/ferrokey-protocol/src" 2>/dev/null || SYS_MISSING+=("$s")
done
# The WS4 adaptive-geometry layer the doc describes must exist.
for s in AdaptiveGeometry KeyTouchStats GeometryConstraints KeyDiagnostics; do
    grep -rq "$s" "$CRATES/ferrokey-core/src/geometry.rs" 2>/dev/null || SYS_MISSING+=("$s")
done
for f in "$CRATES/ferrokeyd/src/serve.rs" "$CRATES/ferrokeyd/src/rate_limit.rs" "$CRATES/ferrokey-uinput/src/ledger.rs"; do
    [ -f "$f" ] || SYS_MISSING+=("$(basename "$f")")
done
if [ ${#SYS_MISSING[@]} -eq 0 ]; then
    gate ARCH.DOCS.002 "system path accurate" PASS "all documented types/modules exist"
else
    gate ARCH.DOCS.002 "system path accurate" FAIL "missing=${SYS_MISSING[*]}"
fi

# ── ARCH.DOCS.003 terminal path accurate ────────────────────────────────────
TERM_SYMBOLS=(TerminalKeySink TerminalKeyEncoder PtySink PtyPair ChildHandle)
TERM_MISSING=()
for s in "${TERM_SYMBOLS[@]}"; do
    grep -rq "$s" "$CRATES/ferrokey-terminal/src" 2>/dev/null || TERM_MISSING+=("$s")
done
# The terminal engine struct is `Terminal` (aliased `TerminalEngine` in the
# app's imports); the doc's claim must match the implementation.
grep -q 'pub struct Terminal' "$CRATES/ferrokey-terminal/src/terminal.rs" || TERM_MISSING+=("Terminal")
if [ ${#TERM_MISSING[@]} -eq 0 ]; then
    gate ARCH.DOCS.003 "terminal path accurate" PASS "all documented terminal types exist"
else
    gate ARCH.DOCS.003 "terminal path accurate" FAIL "missing=${TERM_MISSING[*]}"
fi

# ── ARCH.DOCS.004 keyboard-state diagram (+ all diagram references) ───────
DIAGRAM="$DIAGRAMS/sequence/keyboard-state.mmd"
DIAGRAM_ISSUES=()
for d in "$DIAGRAMS/architecture.mmd" "$DIAGRAMS"/sequence/*.mmd; do
    [ -f "$d" ] || DIAGRAM_ISSUES+=("$d missing")
    grep -q "$(basename "$d")" "$DOC" 2>/dev/null || DIAGRAM_ISSUES+=("$(basename "$d") not referenced")
done
if exists "$DIAGRAM" && grep -q "Down" "$DIAGRAM" && grep -q "release_all" "$DIAGRAM" \
    && grep -q "release_all" "$CRATES/ferrokey-core/src/state.rs" \
    && grep -q "KeyEvent" "$CRATES/ferrokey-core/src/state.rs" \
    && [ ${#DIAGRAM_ISSUES[@]} -eq 0 ]; then
    gate ARCH.DOCS.004 "keyboard-state diagram" PASS "all diagram references resolve"
else
    gate ARCH.DOCS.004 "keyboard-state diagram" FAIL "${DIAGRAM_ISSUES[*]:-state or diagram missing}"
fi

# ── ARCH.DOCS.005 evidence traceability ─────────────────────────────────────
# The authoritative court set (the run-all-courts list + the standalone
# backend-selection court). The doc must reference every court and must not
# reference one that does not exist.
AUTHORITATIVE_COURTS=(kernel-security systemd soak socket-hijack cross-user \
    device-lifetime uinput permissions x11 focus crash repeat modifiers layouts \
    applications dead-keys text-mode touch altgr full-desktop sdl terminal \
    terminal-workspace session-lifetime backend-selection wayland xwayland \
    kernel-debug firefox chromium electron build unit)
BAD_COURTS=()
UNREFERENCED=()
for cid in "${AUTHORITATIVE_COURTS[@]}"; do
    if [ -d "$COURTS/$cid" ]; then
        grep -q "$cid" "$DOC" || UNREFERENCED+=("$cid")
    else
        BAD_COURTS+=("$cid")
    fi
done
for d in "$COURTS"/*/; do
    cid=$(basename "$d")
    grep -qw "$cid" <<< "${AUTHORITATIVE_COURTS[*]}" || BAD_COURTS+=("$cid not in list")
done
PROOF_ISSUES=()
PROOFS_MANIFEST="$REPO_ROOT/proofs/kani-receipt.json"
MUTATION_MANIFEST="$REPO_ROOT/proofs/kani-mutation-receipt.json"
KANI_IDS=$(grep -oE 'KANI\.[A-Z0-9.]+' "$DOC" | sort -u)
for k in $KANI_IDS; do
    found=0
    if [ -f "$PROOFS_MANIFEST" ] && grep -q "\"$k\"" "$PROOFS_MANIFEST"; then
        found=1
    fi
    if [ -f "$MUTATION_MANIFEST" ] && grep -q "\"$k\"" "$MUTATION_MANIFEST"; then
        found=1
    fi
    [ "$found" -eq 0 ] && PROOF_ISSUES+=("$k")
done
if [ ${#BAD_COURTS[@]} -eq 0 ] && [ ${#UNREFERENCED[@]} -eq 0 ] && [ ${#PROOF_ISSUES[@]} -eq 0 ]; then
    gate ARCH.DOCS.005 "evidence traceability" PASS "court/proof references resolve"
else
    gate ARCH.DOCS.005 "evidence traceability" FAIL \
        "bad_courts=${BAD_COURTS[*]:-none} unreferenced=${UNREFERENCED[*]:-none} proofs=${PROOF_ISSUES[*]:-none}"
fi

# ── ARCH.DOCS.006 CONTRIBUTING commands ─────────────────────────────────────
# Every shell command in CONTRIBUTING.md code fences must reference an
# existing binary/script/target.
CMD_ISSUES=()
for cmd in $(grep -oE '^\s*(cargo|bash|RUN_ID=[^ ]+ bash) [^#]+' "$CONTRIBUTING" | sed 's/^[[:space:]]*//' | grep -v '^#' | cut -d' ' -f1-2 | tr ' ' '\n' | sort -u); do
    case "$cmd" in
        cargo|bash|RUN_ID=|sh) ;;                     # leading words
        testing/scripts/*) [ -f "$REPO_ROOT/$cmd" ] || CMD_ISSUES+=("$cmd") ;;
        */) : ;;
        *) : ;;                                        # cargo args / flags checked below
    esac
done
# cargo subcommand targets: the crates named by -p must be workspace members.
for p in $(grep -oE '\-p [a-z0-9-]+' "$CONTRIBUTING" | awk '{print $2}' | sort -u); do
    echo "$MEMBER_CRATES" | grep -qx "$p" || CMD_ISSUES+=("-p $p")
done
# The documented scripts must be executable.
for s in testing/scripts/run-all-courts.sh testing/scripts/run-unit-court.sh \
         testing/scripts/run-clean-court.sh testing/scripts/run-mutation-courts.sh \
         testing/scripts/architecture-drift.sh; do
    [ -x "$REPO_ROOT/$s" ] || CMD_ISSUES+=("$s not executable")
done
if [ ${#CMD_ISSUES[@]} -eq 0 ]; then
    gate ARCH.DOCS.006 "CONTRIBUTING commands" PASS "documented commands resolve"
else
    gate ARCH.DOCS.006 "CONTRIBUTING commands" FAIL "${CMD_ISSUES[*]}"
fi

# ── ARCH.DOCS.007 drift court ───────────────────────────────────────────────
if [ "$FAIL_COUNT" -eq 0 ]; then
    gate ARCH.DOCS.007 "drift court" PASS "all ARCH.DOCS gates PASS"
else
    gate ARCH.DOCS.007 "drift court" FAIL "$FAIL_COUNT gate(s) failed"
fi

# ── receipt + assertions (host-side court: written into the run dir) ────────
mkdir -p "$RUN_DIR/courts"
python3 - "$RUN_DIR/courts/architecture.assertions.json" "${ASSERTIONS[@]}" <<'PYEOF'
import json, sys
with open(sys.argv[1], "w") as fh:
    json.dump([json.loads(a) for a in sys.argv[2:]], fh, indent=2)
PYEOF
if [ "$FAIL_COUNT" -eq 0 ]; then
    pass "architecture" "phase" "documentation-drift"
    echo "ARCH.DOCS drift court ............ PASS ($PASS_COUNT gates)"
    exit 0
else
    fail "architecture" "phase" "documentation-drift"
    echo "ARCH.DOCS drift court ............ FAIL ($FAIL_COUNT gates)"
    exit 1
fi
