# Ferrokey security claims and release gates (Phase 3)

This document states the security claim Ferrokey makes (and what it does
NOT claim), the release gates for Phase 3, and how evidence is generated and
sealed. Claims must match receipts (§115): the security courts generate a
security manifest from observations, never by hand (§90), and hash the
evidence (§91).

## The claim (§115)

> Ferrokey minimizes and enforces its kernel-facing authority: the untrusted
> UI never accesses `/dev/uinput` directly; uinput configuration is isolated
> to startup; the runtime broker runs non-root with zero capabilities,
> NO_NEW_PRIVS, syscall filtering, no network, no physical-input access,
> and an immutable virtual-keyboard capability set; hostile behavior is
> tested against instrumented guest kernels.

What is **not** claimed (§115):

> "Ferrokey can never trigger a Linux kernel vulnerability."

No userspace program interacting with a kernel API can honestly guarantee
that. The correct, engineered and proven claim is:

> A compromised Ferrokey UI cannot expand its kernel-facing authority beyond
> a deliberately tiny, pre-established virtual-keyboard event channel.

## Phase 3 security release gates (§96)

Phase 3 is sealed only when the following all hold inside the disposable VM
security courts (SKIP is never accepted for these — §95):

```text
SEC.PRIV.NON_ROOT ............. PASS
SEC.PRIV.CAPS_EMPTY ........... PASS
SEC.PRIV.NO_NEW_PRIVS ......... PASS

SEC.UINPUT.SINGLE_DEVICE ...... PASS
SEC.UINPUT.CAPABILITY_FIXED ... PASS
SEC.UINPUT.NO_RUNTIME_IOCTL ... PASS
SEC.UINPUT.NO_REOPEN .......... PASS

SEC.DEVICE.NO_PHYSICAL_INPUT .. PASS

SEC.NET.AF_INET_DENIED ........ PASS
SEC.NET.AF_PACKET_DENIED ...... PASS

SEC.SECCOMP.ENFORCED .......... PASS

SEC.PROTOCOL.FUZZ ............. PASS
SEC.PROTOCOL.BOUNDED .......... PASS

SEC.STATE.DISCONNECT_RELEASE .. PASS
SEC.STATE.CRASH_RELEASE ....... PASS

SEC.KERNEL.KASAN .............. PASS
SEC.KERNEL.NO_WARNINGS ........ PASS

SEC.COURT.MUTATION ............ PASS

HOST CONTAMINATION ............ NONE
```

## The hostile court verdict (§117)

The `kernel-security` court family reports, from observations:

```text
BROKER            non-root, zero caps (incl. bounding set), NNP, seccomp
KERNEL INTERFACE  single device, immutable caps, no runtime ioctl,
                  no reopen, no physical input, no unexpected event classes
NETWORK           AF_INET / AF_INET6 / AF_PACKET denied
KERNEL SUBSYSTEMS bpf/perf/ptrace/mount/module/namespace denied
PROTOCOL          invalid opcodes/codes/oversized/partial rejected;
                  fuzz corpus passes; resources bounded
STATE             duplicate down / up-without-down / repeat-without-down
                  safe; disconnect release; SIGKILL recovery; held = {}
KERNEL DEBUG      KASAN/UBSAN/LOCKDEP clean; no WARNING/Oops/panic
COURT INTEGRITY   deliberate mutations detected; failed receipts propagate
HOST              no host contamination
```

## Security manifest (§90)

Each security-court run records (generated from observations):

```json
{
  "ferrokey_commit": "...",
  "kernel": "...",
  "vm_image": "...",
  "euid": 999,
  "effective_capabilities": "0000000000000000",
  "permitted_capabilities": "0000000000000000",
  "ambient_capabilities": "0000000000000000",
  "no_new_privs": true,
  "seccomp": true,
  "uinput_devices": 1,
  "network_families": ["AF_UNIX"],
  "physical_input_access": false,
  "runtime_ioctl_allowed": false,
  "result": "PASS"
}
```

## Evidence integrity (§91)

Receipts, kernel configs, daemon binary hashes, uinput capability dumps,
`/proc/<pid>/status`, FD inventories, the systemd unit, the seccomp policy,
the VM image and the kernel image are hashed and sealed with the run
evidence.

## Failure propagation (§94)

A guest security-court FAIL exits the guest runner non-zero, propagates
through the QEMU/Docker runners and `run-all-courts.sh`, and turns CI red.
No `|| true` masks a failed receipt, and no unconditional `exit 0` hides a
failure. Security receipts are only authoritative once this chain is
verified (checked by `SEC.COURT.MUTATION`).

## Security diagnostics (§104)

```sh
ferrokeyd security-status --pid <broker-pid>
```

prints non-sensitive security state of a running broker — EUID, capability
state, NoNewPrivs, seccomp mode, uinput device identity, configured
capability count and sandbox phase — from `/proc/<pid>/status`. It never
exposes keystrokes or session content. The systemd court records its output
as evidence.
