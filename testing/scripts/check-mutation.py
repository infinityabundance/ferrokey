#!/usr/bin/env python3
"""Verify a SEC.COURT.MUTATION (§93) run caught the mutation on the right gate.

Usage: check-mutation.py <kind> <evidence-dir>

The evidence dir must contain the kernel-security court's receipt.json and
assertions.json. Checks:
  1. the court result is FAIL (the mutation was caught),
  2. the receipt records the expected mutation kind,
  3. every gate the mutation must break is among the FAILed assertions,
  4. no OTHER gate failed (the regression is specific — the mutation did not
     silently break unrelated security properties).

Exits 0 only when all four hold.
"""
import json
import sys

# Mutation kind -> gate label prefixes that MUST have failed. The runner
# documents the contract; these mirror mutations.py.
MUST_FAIL = {
    "run-as-root": ["SEC.PRIV.001", "SEC.PRIV.004", "SEC.MANIFEST"],
    "keep-caps": ["SEC.PRIV.001", "SEC.PRIV.002", "SEC.PRIV.004", "SEC.MANIFEST"],
    "no-nnp": ["SEC.PRIV.003", "SEC.SECCOMP.001", "SEC.MANIFEST"],
    "allow-inet": ["SEC.NET.001", "SEC.SECCOMP.002"],
    "allow-ioctl": ["SEC.SECCOMP.002a", "SEC.SECCOMP.002"],
    "allow-openat": ["SEC.DEVICE.001", "SEC.SECCOMP.002"],
}

# Gates that legitimately fail under a given mutation in addition to the
# MUST_FAIL list (e.g. the probe overall gate when the probe exits non-zero
# is implied by its per-flag gate; there is no other source of failures).
# Failures on any label NOT covered by MUST_FAIL are a contract violation.


def main() -> int:
    if len(sys.argv) != 3:
        sys.stderr.write("usage: check-mutation.py <kind> <evidence-dir>\n")
        return 2
    kind, evdir = sys.argv[1], sys.argv[2]
    if kind not in MUST_FAIL:
        sys.stderr.write(f"unknown mutation kind: {kind}\n")
        return 2

    try:
        receipt = json.load(open(f"{evdir}/receipt.json"))
        assertions = json.load(open(f"{evdir}/assertions.json"))
    except FileNotFoundError as e:
        sys.stderr.write(f"evidence missing: {e}\n")
        return 1

    problems = []

    # 1. The court must have FAILed.
    result = receipt.get("result")
    if result != "FAIL":
        problems.append(f"court result is {result!r}, expected FAIL (mutation not caught)")

    # 2. The receipt must record the mutation kind.
    if receipt.get("mutation") != kind:
        problems.append(
            f"receipt mutation={receipt.get('mutation')!r}, expected {kind!r}"
        )

    failed = {a["assertion"] for a in assertions if a["result"] == "FAIL"}
    passed = {a["assertion"] for a in assertions if a["result"] == "PASS"}

    # 3. Every gate the mutation must break has failed (label prefix match).
    for gate in MUST_FAIL[kind]:
        hit = [label for label in failed if label.startswith(gate)]
        if not hit:
            problems.append(f"gate {gate} did NOT fail under mutation {kind}")
        elif any(label in passed for label in hit):
            problems.append(f"gate {gate} recorded both PASS and FAIL")

    # 4. No failure outside the expected gates. Each gate emits exactly one
    #    ok/bad line, so a FAIL label starting with any other prefix is a
    #    regression the mutation was not supposed to cause.
    allowed = tuple(MUST_FAIL[kind])
    for label in sorted(failed):
        if not label.startswith(allowed):
            problems.append(f"unexpected FAIL on gate not covered by {kind}: {label}")

    if problems:
        sys.stderr.write(f"mutation {kind}: CHECK FAILED\n")
        for p in problems:
            sys.stderr.write(f"  - {p}\n")
        sys.stderr.write(
            f"  failed assertions ({len(failed)}):\n"
            + "\n".join(f"    {l}" for l in sorted(failed))
            + "\n"
        )
        return 1

    print(
        f"mutation {kind}: caught on exactly the expected gate(s) "
        f"({', '.join(MUST_FAIL[kind])})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
