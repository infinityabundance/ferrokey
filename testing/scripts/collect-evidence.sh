#!/usr/bin/env bash
# Collect the evidence pack (rules 38-40, 47): consolidate receipts, build
# the summary + compatibility matrix, seal hashes.
set -euo pipefail
cd "$(dirname "$0")/.."
source scripts/lib.sh

SUMMARY="$RUN_DIR/summary.json"

echo "── evidence run: $RUN_ID"

# Gather all receipts from the run dir.
RECEIPTS=()
for f in "$RUN_DIR"/courts/*.receipt.json; do
    [ -f "$f" ] && RECEIPTS+=("$f")
done

python3 - "$SUMMARY" "${RECEIPTS[@]}" <<'PYEOF'
import json, sys

summary_path = sys.argv[1]
receipts = sys.argv[2:]

results = []
for path in receipts:
    try:
        with open(path) as fh:
            data = json.load(fh)
        results.append(data)
    except Exception as e:
        results.append({"court": path, "result": "ERROR", "error": str(e)})

# Compatibility matrix (rule 47): unknown stays unknown.
matrix_rows = {}
order = ["uinput", "permissions", "x11", "focus", "crash", "repeat",
         "modifiers", "layouts", "applications", "wayland", "xwayland",
         "build", "core"]
for r in results:
    court = r.get("court", "?")
    matrix_rows[court] = r.get("result", "UNKNOWN")

summary = {
    "run_id": results[0].get("run_id") if results else None,
    "ferrokey_commit": results[0].get("ferrokey_commit") if results else None,
    "courts": {c: matrix_rows.get(c, "UNKNOWN") for c in order},
    "result_count": len(results),
    "passed": sum(1 for r in results if r.get("result") == "PASS"),
    "failed": sum(1 for r in results if r.get("result") == "FAIL"),
    "receipts": results,
}
with open(summary_path, "w") as fh:
    json.dump(summary, fh, indent=2)
print(f"summary: {summary['passed']} passed, {summary['failed']} failed, {summary['result_count']} receipts")
PYEOF

# Environment manifest (rule 5).
{
    echo "host: $(uname -srm)"
    echo "docker: $($DOCKER version --format '{{.Server.Version}}' 2>/dev/null || echo unknown)"
    echo "ferrokey_commit: $(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
    echo "rust: $(rustc --version 2>/dev/null || echo n/a)"
} > "$RUN_DIR/environment.json"

# Seal: hash every artifact.
find "$RUN_DIR" -type f | sort | while read -r f; do
    rel="${f#"$RUN_DIR"/}"
    (cd "$RUN_DIR" && sha256sum "$rel") >> "$RUN_DIR/evidence.sha256" 2>/dev/null || true
done

echo "evidence pack sealed at $RUN_DIR (artifacts: $(find "$RUN_DIR" -type f | wc -l))"
