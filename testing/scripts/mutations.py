#!/usr/bin/env python3
"""Mutation definitions for SEC.COURT.MUTATION (§93).

Each mutation applies a *deliberate security regression* to a COPY of the
source tree; the kernel-security court must then FAIL on exactly the gate(s)
that mutation breaks. Proving the court catches each regression proves the
court actually guards the property.

Mutation contract (the court's gate labels that must FAIL under each kind;
the mutation runner verifies these exactly and also verifies no OTHER gate
fails — the regression must be specific):

  run-as-root   -> SEC.PRIV.001 euid != 0      (also 004 bounding, MANIFEST)
                   serve keeps root identity; the daemon's own capset_empty
                   still zeroes caps, so 002 honestly PASSES — the court
                   records that caps ARE empty while euid is 0.
  keep-caps     -> SEC.PRIV.002 caps empty      (also 001/004/MANIFEST)
                   serve keeps root identity AND the daemon's capset_empty +
                   verification are removed, so capabilities are genuinely
                   retained and observed.
  no-nnp        -> SEC.PRIV.003 NoNewPrivs      (also SECCOMP.001, MANIFEST)
                   NO_NEW_PRIVS never set; the seccomp install (which the
                   kernel refuses without NNP) is skipped so the broker still
                   serves — with NNP=0 and Seccomp=0 in /proc.
  allow-inet    -> SEC.NET.001 AF_INET denied   (also SEC.SECCOMP.002)
                   `socket` added to the allowlist; the daemon's internal
                   prove_enforced gate is neutralized so serve runs.
  allow-ioctl   -> SEC.SECCOMP.002a ioctl denied (also SEC.SECCOMP.002)
                   `ioctl` added to the allowlist; internal gate neutralized.
  allow-openat  -> SEC.DEVICE.001 openat denied (also SEC.SECCOMP.002)
                   `openat` added to the allowlist; internal gate neutralized.

Why the internal self-checks are neutralized too: a real daemon fails CLOSED
when a security step fails (§106) — so a mutation that only removes the
enforcement would make serve exit at startup, which the court would catch as
SEC.PRIV.000/phase=serve-pid. That is still a valid catch, but it would not
exercise the court's own per-gate assertions. The mutation simulates a
compromised build where BOTH the enforcement and the daemon-side verification
are removed, forcing the COURT's assertions to be the last line of defense —
exactly the property §93 must prove.

Usage: mutations.py <kind> <source-root>
Applies the patch in place. The caller must run this on a disposable copy
and discard it afterwards; production source is never mutated.
"""
import sys

SEC_RS = "crates/ferrokeyd/src/security.rs"
BOOT_RS = "crates/ferrokeyd/src/bootstrap.rs"
SANDBOX_RS = "crates/ferrokeyd/src/sandbox.rs"
SERVE_RS = "crates/ferrokeyd/src/serve.rs"
SOCKET_RS = "crates/ferrokeyd/src/socket_path.rs"

# Anchor -> replacement pairs. The LAST occurrence of each anchor is replaced
# (production sites only; test modules never contain these exact strings).
MUTATIONS = {
    # serve must not refuse root, and the supervisor must not drop identity.
    # The socket-parent ownership check (refuses a non-root runtime binding
    # into /run/ferrokeyd, owned by the ferrokeyd user) is also neutralized:
    # without it the mutated root broker would fail closed at bind instead of
    # serving as root — the court's privilege gates must be the last line.
    "run-as-root": [
        (
            SEC_RS,
            "pub fn check_refuses_root(allow_root: bool) -> Result<(), SecurityError> {",
            "pub fn check_refuses_root(allow_root: bool) -> Result<(), SecurityError> {\n"
            "        let _ = allow_root; // MUTATION: run-as-root\n"
            "        return Ok(());",
        ),
        (
            BOOT_RS,
            "let serve = security::command_with_dropped_identity(serve, uid, gid);",
            "let serve = serve; // MUTATION: run-as-root - identity not dropped",
        ),
        (
            SOCKET_RS,
            "    if stat.st_uid != euid && stat.st_uid != 0 {",
            "    if false { // MUTATION: run-as-root - parent ownership check neutralized",
        ),
    ],
    # Root identity retained AND the daemon's capability-zeroing + verification
    # removed: the runtime genuinely carries capabilities.
    "keep-caps": [
        (
            BOOT_RS,
            "let serve = security::command_with_dropped_identity(serve, uid, gid);",
            "let serve = serve; // MUTATION: keep-caps - identity not dropped",
        ),
        (
            SEC_RS,
            "pub fn check_refuses_root(allow_root: bool) -> Result<(), SecurityError> {",
            "pub fn check_refuses_root(allow_root: bool) -> Result<(), SecurityError> {\n"
            "        let _ = allow_root; // MUTATION: keep-caps\n"
            "        return Ok(());",
        ),
        (
            SOCKET_RS,
            "    if stat.st_uid != euid && stat.st_uid != 0 {",
            "    if false { // MUTATION: keep-caps - parent ownership check neutralized",
        ),
        (
            SEC_RS,
            "    capset_empty().map_err(SecurityError::Capset)?;",
            "    // MUTATION: keep-caps - capability drop removed",
        ),
        (
            SEC_RS,
            "    if !caps.all_zero() {",
            "    if false { // MUTATION: keep-caps - capability verification neutralized",
        ),
    ],
    # NO_NEW_PRIVS never set; the seccomp install (kernel-required NNP) is
    # skipped and the internal probe gate neutralized so the broker serves.
    "no-nnp": [
        (
            SEC_RS,
            "    set_no_new_privs().map_err(SecurityError::NoNewPrivs)?;",
            "    // MUTATION: no-nnp - NO_NEW_PRIVS not set",
        ),
        (
            SEC_RS,
            "    if !no_new_privs_active().map_err(SecurityError::NoNewPrivs)? {",
            "    if false { // MUTATION: no-nnp - verification neutralized",
        ),
        (
            SERVE_RS,
            "    sandbox::install_filter().map_err(ServeError::Seccomp)?;",
            "    // MUTATION: no-nnp - seccomp install skipped (kernel requires NO_NEW_PRIVS)",
        ),
        (
            SERVE_RS,
            "    if !probes.all_denied() {",
            "    if false { // MUTATION: no-nnp - internal probe gate neutralized",
        ),
    ],
    # Each allow-* mutation adds the syscall to the x86_64 allowlist and
    # neutralizes the daemon's internal prove_enforced gate so serve runs with
    # the weakened filter; the court's sandbox-probe assertions catch it.
    "allow-inet": [
        (
            SANDBOX_RS,
            "    288, // accept4\n    318, // getrandom",
            "    288, // accept4\n    318, // getrandom\n"
            "    41, // socket - MUTATION: allow-inet",
        ),
        (
            SERVE_RS,
            "    if !probes.all_denied() {",
            "    if false { // MUTATION: allow-inet - internal probe gate neutralized",
        ),
    ],
    "allow-ioctl": [
        (
            SANDBOX_RS,
            "    288, // accept4\n    318, // getrandom",
            "    288, // accept4\n    318, // getrandom\n"
            "    16, // ioctl - MUTATION: allow-ioctl",
        ),
        (
            SERVE_RS,
            "    if !probes.all_denied() {",
            "    if false { // MUTATION: allow-ioctl - internal probe gate neutralized",
        ),
    ],
    "allow-openat": [
        (
            SANDBOX_RS,
            "    288, // accept4\n    318, // getrandom",
            "    288, // accept4\n    318, // getrandom\n"
            "    257, // openat - MUTATION: allow-openat",
        ),
        (
            SERVE_RS,
            "    if !probes.all_denied() {",
            "    if false { // MUTATION: allow-openat - internal probe gate neutralized",
        ),
    ],
}


def apply_mutation(kind: str, root: str) -> None:
    if kind not in MUTATIONS:
        sys.stderr.write(f"unknown mutation kind: {kind}\n")
        sys.exit(2)
    for rel, old, new in MUTATIONS[kind]:
        path = f"{root}/{rel}"
        with open(path) as fh:
            text = fh.read()
        if old not in text:
            sys.stderr.write(f"mutation {kind}: anchor not found in {rel}\n")
            sys.exit(3)
        # Replace the LAST occurrence (the production site, not any test copy).
        idx = text.rindex(old)
        text = text[:idx] + text[idx:].replace(old, new, 1)
        with open(path, "w") as fh:
            fh.write(text)
    print(f"mutation {kind} applied")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.stderr.write("usage: mutations.py <kind> <source-root>\n")
        sys.exit(2)
    apply_mutation(sys.argv[1], sys.argv[2])
