#!/usr/bin/env bash
# GENERATE THE COMPATIBILITY RECEIPT (addendum §37, §67).
#
# The receipt is GENERATED from actual court evidence — it is never
# hand-edited. Each row is backed by the receipts and (where present) the
# per-assertion logs that the courts wrote during the VM runs.
#
# Inputs:
#   - docker volume evidence:   /court/state/evidence/<court>/{receipt,assertions}.json
#   - docker-only receipts:     $RUN_DIR/courts/*.receipt.json  (build/unit/clean)
#
# Outputs:
#   - $RUN_DIR/compatibility-receipt.json   machine-readable receipt
#   - $RUN_DIR/compatibility-receipt.md     §37 human-readable statement
#   - /court/state/compatibility-receipt.{json,md}   volume copy ("latest")
#
# Usage:
#   bash testing/scripts/generate-compat-receipt.sh [RUN_ID]
set -euo pipefail

# Capture the run before sourcing (scripts/lib.sh computes RUN_ID/RUN_DIR
# eagerly from the current time).
PRE_RUN_ID="${RUN_ID:-${1:-}}"
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

if [ -n "$PRE_RUN_ID" ]; then
    RUN_ID="$PRE_RUN_ID"
else
    RUN_ID=$(ls -1dt "$TESTING_DIR"/evidence/*/ 2>/dev/null | head -1 | xargs basename 2>/dev/null || echo latest)
fi
RUN_DIR="$TESTING_DIR/evidence/$RUN_ID"
mkdir -p "$RUN_DIR"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "── compatibility receipt: run $RUN_ID"

# ---------------------------------------------------------------------------
# 1. Dump the docker-volume evidence for every VM court in one container pass.
# ---------------------------------------------------------------------------
# The oracle entrypoint is /bin/bash, so the args after the image name are
# the script itself (`-c '...'`), not `bash -c '...'`.
"$DOCKER" run --rm --network host -v ferrokey-vm-state:/court/state \
    "$ORACLE_IMAGE" -c '
set -u
cd /court/state/evidence 2>/dev/null || exit 0
for d in */; do
    court="${d%/}"
    [ -f "$d/receipt.json" ] || continue
    echo "===COURT:$court"
    cat "$d/receipt.json"
    # cat does not append a newline; the marker that follows must start its
    # own line or the section parser swallows it (assertions.json, unlike
    # the jq-written receipt, has no trailing newline).
    echo
    if [ -f "$d/assertions.json" ]; then
        echo "===ASSERTIONS:$court"
        cat "$d/assertions.json"
        echo
    fi
    if [ -f "$d/result" ]; then
        echo "===RESULT:$court"
        cat "$d/result"
        echo
    fi
done
' > "$TMP/vm-evidence.txt" 2>/dev/null || true

# ---------------------------------------------------------------------------
# 2. Generate the receipt from evidence + the docker-only run-dir receipts.
# ---------------------------------------------------------------------------
RECEIPT_JSON="$RUN_DIR/compatibility-receipt.json"
RECEIPT_MD="$RUN_DIR/compatibility-receipt.md"

python3 - "$TMP/vm-evidence.txt" "$RECEIPT_JSON" "$RECEIPT_MD" \
    "$RUN_ID" "$RUN_DIR/courts" <<'PYEOF'
import json, os, sys
import datetime

evidence_path, json_path, md_path = sys.argv[1], sys.argv[2], sys.argv[3]
run_id, courts_dir = sys.argv[4], sys.argv[5]

# --- parse the dumped VM evidence -------------------------------------------
vm = {}
current, mode, buf = None, None, []


def parse_json(buf):
    try:
        return json.loads("\n".join(buf))
    except Exception:
        return None


def commit():
    global mode, buf
    if current is None:
        return
    entry = vm.setdefault(current, {"receipt": {}, "assertions": [], "result": None})
    if mode == "receipt":
        entry["receipt"] = parse_json(buf) or {}
    elif mode == "assertions":
        entry["assertions"] = parse_json(buf) or []
    elif mode == "result":
        entry["result"] = "".join(buf).strip() or None
    # `current` deliberately persists: the ASSERTIONS/RESULT sections belong
    # to the most recent COURT section and only the next COURT marker (or the
    # end of the stream) closes the record.
    mode, buf = None, []


with open(evidence_path) as fh:
    for raw in fh:
        line = raw.rstrip("\n")
        if line.startswith("===COURT:"):
            commit()
            current = line[len("===COURT:"):]
            mode = "receipt"
            buf = []
        elif line.startswith("===ASSERTIONS:"):
            commit()
            mode = "assertions"
            buf = []
        elif line.startswith("===RESULT:"):
            commit()
            mode = "result"
            buf = []
        else:
            buf.append(line)
commit()

# --- docker-only receipts (build / unit / clean) on the host --------------
# CI runs each court as its own step, so receipts can live in several run
# dirs; ingest all of them and keep the most recent receipt per court name
# (newest timestamp wins, ties broken in favour of the named run dir).
docker_receipts = {}


def ingest_courts_dir(rd):
    if not os.path.isdir(rd):
        return
    for f in sorted(os.listdir(rd)):
        if not f.endswith(".receipt.json"):
            continue
        try:
            with open(os.path.join(rd, f)) as fh:
                r = json.load(fh)
        except Exception:
            continue
        court = r.get("court", f[:-len(".receipt.json")])
        old = docker_receipts.get(court)
        if old is None or str(r.get("timestamp", "")) >= str(old.get("timestamp", "")):
            docker_receipts[court] = r


if os.path.isdir(courts_dir):
    ingest_courts_dir(courts_dir)
for run in sorted(os.listdir(os.path.dirname(courts_dir) or ".")):
    ingest_courts_dir(os.path.join(os.path.dirname(courts_dir), run, "courts"))

def court_result(court):
    """PASS/FAIL/UNKNOWN for a court, from either evidence source."""
    if court in vm and vm[court]["receipt"].get("result"):
        return vm[court]["receipt"]["result"]
    if court in docker_receipts and docker_receipts[court].get("result"):
        return docker_receipts[court]["result"]
    return "UNKNOWN"

def court_assertions(court):
    if court in vm and vm[court]["assertions"]:
        return vm[court]["assertions"]
    return None

def court_failures(court):
    a = court_assertions(court)
    if a is None:
        return None
    return [x for x in a if x.get("result") == "FAIL"]

# --- the §37 statement rows (section, row label, backing courts) ------------
# Sections: surfaces, targets, keystate, build. Derived rows carry their own
# section. Grouping is explicit so the human-readable statement never depends
# on list positions.
rows = [
    ("surfaces", "Linux kernel keyboard injection",          ["uinput"]),
    ("surfaces", "Daemon security boundary / permissions",   ["permissions"]),
    ("surfaces", "X11 no-focus surface",                     ["x11"]),
    ("surfaces", "XWayland no-focus surface",                ["xwayland"]),
    ("surfaces", "wlroots / layer-shell (Wayland)",          ["wayland"]),
    ("surfaces", "Backend selection policy (§65/§66)",         ["backend-selection"]),
    ("surfaces", "Focus preservation (applications)",        ["focus"]),
    ("surfaces", "GTK / Qt / Slint / X11 targets",           ["applications"]),
    ("targets",  "Firefox",                                  ["firefox"]),
    ("targets",  "Chromium",                                 ["chromium"]),
    ("targets",  "Electron",                                 ["electron"]),
    ("targets",  "Terminal (external xterm target)",        ["terminal"]),
    ("targets",  "Embedded terminal workspace (OSK→PTY)",    ["terminal-workspace"]),
    ("targets",  "SDL",                                      ["sdl"]),
    ("keystate", "Modifiers",                                ["modifiers"]),
    ("keystate", "Autorepeat",                               ["repeat"]),
    ("keystate", "Layouts",                                  ["layouts"]),
    ("keystate", "Dead keys / compose",                      ["dead-keys"]),
    ("keystate", "AltGr",                                    ["altgr"]),
    ("keystate", "Text mode",                                ["text-mode"]),
    ("keystate", "Touch / pen",                              ["touch"]),
    ("keystate", "Full-desktop key coverage",                ["full-desktop"]),
    ("keystate", "Crash recovery / release-all",             ["crash"]),
    ("security", "Broker privilege state (non-root, zero caps, NNP)", ["kernel-security"]),
    ("security", "Seccomp enforcement + device immutability", ["kernel-security"]),
    ("security", "Network / ioctl / device-open denial",      ["kernel-security"]),
    ("security", "Systemd unit hardening",                    ["systemd"]),
    ("security", "Long-run soak stability",                   ["soak"]),
    ("security", "Socket-path hijack resistance (§101)",      ["socket-hijack"]),
    ("security", "Cross-user authorization (§100)",            ["cross-user"]),
    ("security", "Session-scope binding (§28, §99)",            ["session-lifetime"]),
    ("security", "Device lifetime / restart (§73)",            ["device-lifetime"]),
    ("security", "KASAN+UBSAN+LOCKDEP kernel court (§66–§68)", ["kernel-debug"]),
    ("security", "Deliberate regression mutations (§93)",     ["mutation"]),
    ("build",    "Workspace build + test + clippy + fmt",    ["build.workspace"]),
    ("build",    "Core unit tests",                          ["core.unit"]),
    ("build",    "Clean build from empty caches",            ["build.clean"]),
    ("docs",     "Architecture documentation drift court",   ["architecture"]),
]

def worst(results):
    if not results:
        return "UNKNOWN"
    if any(r == "FAIL" for r in results):
        return "FAIL"
    if all(r == "PASS" for r in results):
        return "PASS"
    return "UNKNOWN"

statement = []
for section, label, courts in rows:
    results = [court_result(c) for c in courts]
    failures = [f for c in courts for f in (court_failures(c) or [])]
    n_assertions = sum(len(court_assertions(c) or []) for c in courts)
    statement.append({
        "section": section,
        "row": label,
        "courts": courts,
        "result": worst(results),
        "assertions": n_assertions,
        "failed_assertions": [f.get("assertion") for f in failures],
    })

# Derived rows from assertion content (addendum §37: chords, stuck keys).
def derived(section, label, pattern):
    hits, fails = [], 0
    for court in vm:
        for a in vm[court]["assertions"]:
            if pattern in a.get("assertion", "").lower():
                hits.append((court, a))
                if a.get("result") == "FAIL":
                    fails += 1
    result = "UNKNOWN"
    if hits:
        result = "FAIL" if fails else "PASS"
    return {
        "section": section,
        "row": label,
        "courts": sorted({c for c, _ in hits}),
        "result": result,
        "assertions": len(hits),
        "failed_assertions": [a.get("assertion") for c, a in hits if a.get("result") == "FAIL"],
    }

statement.append(derived("keystate", "Chord ordering (modifiers court, full-desktop, SDL)", "chord"))
statement.append(derived("keystate", "Stuck keys", "stuck"))

# --- assemble the receipt ----------------------------------------------------
commit = "unknown"
for r in list(docker_receipts.values()):
    if r.get("ferrokey_commit"):
        commit = r["ferrokey_commit"]
        break

meta = {"run_id": run_id, "ferrokey_commit": commit,
        "generated_at": datetime.datetime.now(datetime.timezone.utc)
                         .strftime("%Y-%m-%dT%H:%M:%SZ")}

courts_out = {}
for c in sorted(set(list(vm.keys()) + list(docker_receipts.keys()))):
    r = vm.get(c, {}).get("receipt") or docker_receipts.get(c) or {}
    courts_out[c] = {
        "result": court_result(c),
        "kernel": r.get("kernel"),
        "distro": r.get("distro"),
        "assertions": (len(court_assertions(c)) if court_assertions(c) is not None else None),
        "failed_assertions": [f.get("assertion") for f in (court_failures(c) or [])],
    }

receipt = {
    "schema": "ferrokey-compatibility-receipt/1",
    "meta": meta,
    "courts": courts_out,
    "statement": statement,
    "summary": {
        "passed": sum(1 for s in statement if s["result"] == "PASS"),
        "failed": sum(1 for s in statement if s["result"] == "FAIL"),
        "unknown": sum(1 for s in statement if s["result"] == "UNKNOWN"),
        "total_assertions": sum(s["assertions"] for s in statement),
    },
}
with open(json_path, "w") as fh:
    json.dump(receipt, fh, indent=2)

# --- §37 human-readable statement -------------------------------------------
md = []
md.append("FERROKEY COMPATIBILITY COURT")
md.append("")
md.append(f"Generated: {meta['generated_at']}   Run: {run_id}")
md.append(f"Commit: {commit}")
md.append("")
md.append("SURFACES")
md.append("")
for s in statement:
    if s["section"] == "surfaces":
        md.append(f"{s['row']}:".ljust(46) + f" {s['result']}")
md.append("")
md.append("TARGETS")
md.append("")
for s in statement:
    if s["section"] == "targets":
        md.append(f"{s['row']}:".ljust(46) + f" {s['result']}")
md.append("")
md.append("KEY STATE")
md.append("")
for s in statement:
    if s["section"] == "keystate":
        md.append(f"{s['row']}:".ljust(46) + f" {s['result']}")
md.append("")
md.append("BUILD")
md.append("")
for s in statement:
    if s["section"] == "build":
        md.append(f"{s['row']}:".ljust(46) + f" {s['result']}")
md.append("")
md.append("SECURITY")
md.append("")
for s in statement:
    if s["section"] == "security":
        md.append(f"{s['row']}:".ljust(46) + f" {s['result']}")
md.append("")
md.append(f"Assertion rows recorded: {sum(s['assertions'] for s in statement)}")
md.append("")
fails = [s for s in statement if s["result"] == "FAIL"]
if fails:
    md.append("FAILING ROWS")
    md.append("")
    for s in fails:
        md.append(f"- {s['row']}: {s['failed_assertions']}")
md.append("")
md.append("_Generated from court evidence; do not hand-edit._")
with open(md_path, "w") as fh:
    fh.write("\n".join(md) + "\n")

print(f"receipt: {receipt['summary']['passed']} passed, "
      f"{receipt['summary']['failed']} failed, "
      f"{receipt['summary']['unknown']} unknown "
      f"({receipt['summary']['total_assertions']} assertion rows)")
PYEOF

# ---------------------------------------------------------------------------
# 3. Copy the receipt into the docker volume ("latest" pointer for
#    container-side consumers).
# ---------------------------------------------------------------------------
"$DOCKER" run --rm --network host -v ferrokey-vm-state:/court/state \
    -v "$RUN_DIR:/in:ro" \
    "$ORACLE_IMAGE" -c \
    'cp /in/compatibility-receipt.json /in/compatibility-receipt.md /court/state/ 2>/dev/null || true'

echo "compatibility receipt: $RUN_DIR/compatibility-receipt.{json,md}"
