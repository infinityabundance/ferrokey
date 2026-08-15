#!/usr/bin/env bash
# MAN — the man-page documentation court (Phase 4 WS2, §2.6/§2.7).
#
# Gates MAN.001–008:
#   MAN.001-004  ferrokey(1), ferrokeyd(1), ferrokey.yaml(5), ferrokeyd.yaml(5)
#                render (groff -man) — via `cargo xtask man`
#   MAN.005      CLI coverage: every real public option is documented, every
#                documented option is real (source parsers vs troff pages)
#   MAN.006      config coverage: every real config field is documented,
#                every documented field is real (serde structs vs pages)
#   MAN.007      examples parse through the real config parsers (xtask)
#   MAN.008      the drift control itself passes
#
#   bash testing/scripts/man-drift.sh [RUN_ID]
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

MAN_DIR="$REPO_ROOT/docs/man"
PASS_COUNT=0
FAIL_COUNT=0
ASSERTIONS=()

gate() {
    local id="$1" label="$2" result="$3" detail="$4"
    if [ "$result" = PASS ]; then
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
    printf '%-14s %-48s %s\n' "$id" "$label" "$result"
    if [ "$result" != PASS ]; then
        printf '  detail: %s\n' "$detail"
    fi
    ASSERTIONS+=("{\"assertion\": \"$id $label\", \"result\": \"$result\", \"detail\": \"$detail\"}")
}

# ── render + example parse (MAN.001-004, MAN.007) ───────────────────────────
XTSASK_OUT=$(cargo xtask man 2>&1) || XTSASK_RC=$?
XTSASK_RC=${XTSASK_RC:-0}
if [ "$XTSASK_RC" -eq 0 ]; then
    for page in ferrokey.1 ferrokeyd.1 ferrokey.yaml.5 ferrokeyd.yaml.5; do
        if [ -s "$MAN_DIR/out/$page.txt" ]; then
            gate "MAN.001" "$page renders" PASS "groff -man -> docs/man/out/$page.txt"
        else
            gate "MAN.001" "$page renders" FAIL "rendered output missing"
        fi
    done
    gate "MAN.007" "examples parse" PASS "UiConfig/DaemonConfig parse the documented examples"
else
    for page in ferrokey.1 ferrokeyd.1 ferrokey.yaml.5 ferrokeyd.yaml.5; do
        gate "MAN.001" "$page renders" FAIL "cargo xtask man failed"
    done
    gate "MAN.007" "examples parse" FAIL "cargo xtask man failed"
fi

# ── CLI coverage (MAN.005) ──────────────────────────────────────────────────
# Real options: the match arms of the arg parsers (ferrokey UI + ferrokeyd).
REAL_CLI=$(grep -hoE '"(--[a-z][a-z-]*)"' "$REPO_ROOT/src/main.rs" \
    "$REPO_ROOT/crates/ferrokeyd/src/main.rs" 2>/dev/null | tr -d '"' | sort -u)
# Documented options: unescape the troff dashes first, then take --flags.
DOC_CLI=$(sed 's/\\-/-/g' "$MAN_DIR"/*.1 2>/dev/null | grep -ohE -- '--[a-z][a-z-]*' | sort -u)
UNDOCUMENTED=()
for f in $REAL_CLI; do
    case "$f" in --help|-h|--version|-V) continue ;; esac   # boilerplate
    echo "$DOC_CLI" | grep -qx -- "$f" || UNDOCUMENTED+=("$f")
done
GHOST=()
for f in $DOC_CLI; do
    case "$f" in --help|-h|--version|-V) continue ;; esac
    echo "$REAL_CLI" | grep -qx -- "$f" || GHOST+=("$f")
done
if [ ${#UNDOCUMENTED[@]} -eq 0 ] && [ ${#GHOST[@]} -eq 0 ]; then
    gate "MAN.005" "CLI coverage" PASS "$(echo "$REAL_CLI" | wc -l) real options all documented"
else
    gate "MAN.005" "CLI coverage" FAIL "undocumented=${UNDOCUMENTED[*]:-none} ghost=${GHOST[*]:-none}"
fi

# ── config coverage (MAN.006) ───────────────────────────────────────────────
# Real fields: pub fields of the serde config structs.
REAL_CFG=$(grep -hoE 'pub [a-z_0-9]+:' "$REPO_ROOT/src/config.rs" \
    "$REPO_ROOT/crates/ferrokeyd/src/config.rs" 2>/dev/null | sed -E 's/pub ([a-z_0-9]+):/\1/' | sort -u)
# Documented fields: every `.B field` / `.B parent.field` token in the .5
# pages (skipping `.B \-\-flag` references in prose).
DOC_CFG=$(sed -n 's/^\.B //p' "$MAN_DIR"/*.5 2>/dev/null \
    | grep -vE '^(\\-\\-|--)' \
    | grep -oE '[a-z_][a-z_0-9]*(\.[a-z_][a-z_0-9]*)?' | awk -F. '{print $NF}' | sort -u)
CFG_UNDOCUMENTED=()
for f in $REAL_CFG; do
    echo "$DOC_CFG" | grep -qx -- "$f" || CFG_UNDOCUMENTED+=("$f")
done
CFG_GHOST=()
for f in $DOC_CFG; do
    echo "$REAL_CFG" | grep -qx -- "$f" || CFG_GHOST+=("$f")
done
if [ ${#CFG_UNDOCUMENTED[@]} -eq 0 ] && [ ${#CFG_GHOST[@]} -eq 0 ]; then
    gate "MAN.006" "config coverage" PASS "$(echo "$REAL_CFG" | wc -l) real fields all documented"
else
    gate "MAN.006" "config coverage" FAIL "undocumented=${CFG_UNDOCUMENTED[*]:-none} ghost=${CFG_GHOST[*]:-none}"
fi

# ── the drift control itself (MAN.008) ──────────────────────────────────────
if [ "$FAIL_COUNT" -eq 0 ]; then
    gate "MAN.008" "drift control" PASS "all MAN gates PASS"
else
    gate "MAN.008" "drift control" FAIL "$FAIL_COUNT gate(s) failed"
fi

mkdir -p "$RUN_DIR/courts"
python3 - "$RUN_DIR/courts/man.assertions.json" "${ASSERTIONS[@]}" <<'PYEOF'
import json, sys
with open(sys.argv[1], "w") as fh:
    json.dump([json.loads(a) for a in sys.argv[2:]], fh, indent=2)
PYEOF
if [ "$FAIL_COUNT" -eq 0 ]; then
    pass "man" "phase" "man-pages"
    echo "MAN drift court ................... PASS ($PASS_COUNT gates)"
    exit 0
else
    fail "man" "phase" "man-pages"
    echo "MAN drift court ................... FAIL ($FAIL_COUNT gates)"
    exit 1
fi
