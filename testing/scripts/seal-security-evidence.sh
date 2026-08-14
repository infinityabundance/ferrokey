#!/usr/bin/env bash
# SEC evidence seal (§90, §91, §96): aggregate every SEC.* gate observed in
# this run into security-summary.json + a §96 human-readable
# security-receipt.md, derived from the run-dir evidence — never hand-built.
#
#   bash testing/scripts/seal-security-evidence.sh [RUN_ID]
#
# Inputs (in the run dir):
#   courts/<court>/receipt.json + assertions.json   (pulled from the VM
#                                                    state volume per court)
#   mutations/<kind>/courts/kernel-security/…       (mutation courts, §93)
#   mutations/results.json                          (mutation runner summary)
#
# Outputs:
#   security-summary.json      machine-readable §96 gate matrix
#   security-receipt.md        §117-style human-readable statement
#
# A gate that cannot be observed is SKIP — never PASS (§95).
set -euo pipefail
# Capture the run before sourcing (scripts/lib.sh computes RUN_ID/RUN_DIR
# eagerly from the current time).
PRE_RUN_ID="${RUN_ID:-${1:-}}"
cd "$(dirname "$0")/.."
source scripts/lib.sh
sanitize_env

if [ -z "$PRE_RUN_ID" ]; then
    PRE_RUN_ID=$(ls -1dt "$TESTING_DIR"/evidence/*/ 2>/dev/null | head -1 | xargs basename 2>/dev/null || echo latest)
fi
RUN_DIR="$TESTING_DIR/evidence/$PRE_RUN_ID"
mkdir -p "$RUN_DIR"

echo "── security evidence seal: run $PRE_RUN_ID"

python3 - "$RUN_DIR" <<'PYEOF'
import json, os, sys
import datetime

run_dir = sys.argv[1].rstrip("/")
run_id = os.path.basename(run_dir)

def load(p, default=None):
    try:
        with open(p) as fh:
            return json.load(fh)
    except Exception:
        return default

def court_dir(name):
    return os.path.join(run_dir, "courts", name)

# ── gather evidence ────────────────────────────────────────────────────────
# Each court: receipt + assertion list (label prefix -> result).
courts = {}
for name in sorted(os.listdir(os.path.join(run_dir, "courts"))):
    d = os.path.join(run_dir, "courts", name)
    if not os.path.isdir(d):
        continue
    receipt = load(os.path.join(d, "receipt.json")) or {}
    assertions = load(os.path.join(d, "assertions.json")) or []
    courts[name] = {"receipt": receipt, "assertions": assertions}

def gate_ok(gate, court, *labels):
    """A gate passes when every backing assertion label is PASS. If the
    backing court or any label is missing, the gate is SKIP (§95)."""
    c = courts.get(court)
    if not c:
        return "SKIP", f"court {court} not run"
    if not labels:
        return "SKIP", f"no assertions mapped for {gate}"
    results = set()
    for lab in labels:
        hits = [a for a in c["assertions"] if a.get("assertion", "").startswith(lab)]
        if not hits:
            return "SKIP", f"{court}: no assertion '{lab}*'"
        results.update(a["result"] for a in hits)
    if "FAIL" in results:
        return "FAIL", f"{court}: {sorted(labels)} contains a FAIL"
    return "PASS", f"{court}: {sorted(labels)} all PASS"

# Mutation summary: the mutation runner writes results.json (§93).
mutation_result = "SKIP"
mutation_detail = "no mutation evidence"
mutations_dir = os.path.join(run_dir, "mutations")
mut_results = load(os.path.join(mutations_dir, "results.json"))
if mut_results:
    kinds = mut_results.get("kinds", [])
    if mut_results.get("all_caught"):
        mutation_result = "PASS"
        mutation_detail = f"all {len(kinds)} mutations caught: {kinds}"
    else:
        mutation_result = "FAIL"
        mutation_detail = f"not all mutations caught: {kinds}"

# Host contamination: the postflight writes its verdict into the run dir.
contamination = "UNKNOWN"
pf = os.path.join(run_dir, "host-contamination.txt")
if os.path.exists(pf):
    contamination = open(pf).read().strip()

# ── §96 gate matrix ─────────────────────────────────────────────────────────
gates = []
def add(gate, result, detail):
    gates.append({"gate": gate, "result": result, "detail": detail})

rows = [
    ("SEC.PRIV.NON_ROOT",         "kernel-security", ("SEC.PRIV.001",)),
    ("SEC.PRIV.CAPS_EMPTY",       "kernel-security", ("SEC.PRIV.002",)),
    ("SEC.PRIV.NO_NEW_PRIVS",     "kernel-security", ("SEC.PRIV.003",)),
    ("SEC.UINPUT.SINGLE_DEVICE",  "kernel-security", ("SEC.UINPUT.SINGLE_DEVICE",)),
    ("SEC.UINPUT.CAPABILITY_FIXED", "kernel-security", ("SEC.UINPUT.CAPABILITY_FIXED",)),
    ("SEC.UINPUT.NO_RUNTIME_IOCTL", "kernel-security", ("SEC.SECCOMP.002a",)),
    ("SEC.UINPUT.NO_REOPEN",      "kernel-security", ("SEC.DEVICE.001",)),
    ("SEC.DEVICE.NO_PHYSICAL_INPUT", "kernel-security", ("SEC.DEVICE.001b",)),
    ("SEC.NET.AF_INET_DENIED",    "kernel-security", ("SEC.NET.001",)),
    ("SEC.NET.AF_PACKET_DENIED",  "kernel-security", ("SEC.NET.001c",)),
    ("SEC.SECCOMP.ENFORCED",      "kernel-security", ("SEC.SECCOMP.001", "SEC.SECCOMP.002")),
    ("SEC.PROTOCOL.FUZZ",         "kernel-security", ("SEC.PROTOCOL.FUZZ",)),
    ("SEC.PROTOCOL.BOUNDED",      "kernel-security", ("SEC.FD.001",)),
    ("SEC.STATE.DISCONNECT_RELEASE", "kernel-security", ("SEC.STATE.DISCONNECT_RELEASE",)),
    ("SEC.STATE.CRASH_RELEASE",   "kernel-security", ("SEC.STATE.SIGKILL",)),
    ("SEC.KERNEL.NO_WARNINGS",    "kernel-security", ("SEC.KERNEL.NO_WARNINGS",)),
    ("SEC.KERNEL.KASAN",          "kernel-debug", ("SEC.KERNEL.KASAN_CLEAN",)),
    ("SEC.SYSTEMD.HARDENED",      "systemd", ("SEC.SYSTEMD.001", "SEC.SYSTEMD.008")),
    ("SEC.SOAK.BOUNDED",          "soak", ("SEC.SOAK.001", "SEC.SOAK.002", "SEC.SOAK.003")),
    ("SEC.SESSION.BOUND",         "session-lifetime", ("SESSION.001", "SESSION.002", "SESSION.003", "SESSION.004")),
]
for gate, court, labels in rows:
    result, detail = gate_ok(gate, court, *labels)
    add(gate, result, detail)
add("SEC.COURT.MUTATION", mutation_result, mutation_detail)

# Host contamination gate from the postflight verdict.
add("HOST_CONTAMINATION", "PASS" if contamination in ("NONE", "PASS") else "FAIL", f"postflight: {contamination}")

summary = {
    "run_id": run_id,
    "generated_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    "courts": {n: c["receipt"].get("result", "UNKNOWN") for n, c in sorted(courts.items())},
    "gates": gates,
    "result": "FAIL" if any(g["result"] == "FAIL" for g in gates) else (
              "PASS" if all(g["result"] == "PASS" for g in gates) else "SKIP-SOME"),
}
with open(os.path.join(run_dir, "security-summary.json"), "w") as fh:
    json.dump(summary, fh, indent=2)

# ── §117-style human-readable statement ────────────────────────────────────
md = []
md.append("FERROKEY SECURITY COURT — HOSTILE AUDIT")
md.append("")
md.append(f"Run: {summary['run_id']}   Generated: {summary['generated_at']}")
md.append("")
for g in gates:
    md.append(f"{g['gate']}:".ljust(38) + f" {g['result']}")
md.append("")
skipped = [g for g in gates if g["result"] == "SKIP"]
if skipped:
    md.append("SKIPPED GATES (SKIP != PASS, §95)")
    for g in skipped:
        md.append(f"- {g['gate']}: {g['detail']}")
md.append("")
md.append("_Derived from court evidence; do not hand-edit._")
with open(os.path.join(run_dir, "security-receipt.md"), "w") as fh:
    fh.write("\n".join(md) + "\n")

print(f"security seal: {sum(1 for g in gates if g['result']=='PASS')} PASS, "
      f"{sum(1 for g in gates if g['result']=='FAIL')} FAIL, "
      f"{sum(1 for g in gates if g['result']=='SKIP')} SKIP")
PYEOF

# The security manifest from the kernel-security court is the observation
# record (§90); surface it at the run root too.
if [ -f "$RUN_DIR/courts/kernel-security/security-manifest.json" ]; then
    cp "$RUN_DIR/courts/kernel-security/security-manifest.json" "$RUN_DIR/security-manifest.json"
fi

echo "security seal: $RUN_DIR/security-summary.json + security-receipt.md"
