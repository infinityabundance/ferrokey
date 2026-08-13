# Ferrokey kernel attack-surface threat model

Phase 3 of Ferrokey hardens the on-screen keyboard so that it cannot become a
useful kernel attack surface, privilege-escalation path, arbitrary
device-access path, or general-purpose local injection broker.

This document describes the kernel interfaces reachable before and after the
sandbox, the syscalls and file descriptors reachable at runtime, the protocol
operations, the trusted computing base, and the known limitations. It is kept
aligned with the code and the security courts (`testing/courts/kernel-security/`).

## Trust ranking (§108)

```text
UNTRUSTED
    Ferrokey UI (Slint)
    layout files, themes, user configuration
    IPC bytes on the daemon socket

SMALL TRUSTED COMPUTING BASE
    broker runtime (ferrokeyd serve)
    protocol decoder
    key validator
    held-key ledger
    sandbox initialization

TINY INITIALIZATION TCB
    uinput device bootstrap (ferrokeyd init)

EXTERNAL TRUST
    Linux kernel
    systemd / logind where used
```

The UI is **assumed compromised** for the security analysis: an attacker with
arbitrary code execution in the UI gains exactly the kernel-facing authority
described under "After the sandbox" below — and nothing more.

## Architecture

```text
UNTRUSTED UI
     │
     │ tiny authenticated bounded protocol (FK01 v2, AF_UNIX, SO_PEERCRED)
     ▼
FERROKEYD RUNTIME BROKER (ferrokeyd serve)
     │  non-root, zero capabilities, NO_NEW_PRIVS, seccomp allowlist
     │  no network, no arbitrary open(), no runtime ioctl
     │  one pre-created uinput keyboard (verified identity + capability set)
     ▼
write(input_event)  ──▶  Linux uinput  ──▶  Linux input subsystem
```

The runtime broker is spawned by a tiny supervisor (`ferrokeyd start`) that
first runs the bootstrap component (`ferrokeyd init`): init opens
`/dev/uinput`, configures exactly the immutable capability set, creates ONE
virtual keyboard, verifies its identity and capability bitmap from sysfs, and
transfers the fd to the runtime over a private `SOCK_SEQPACKET` socketpair via
SCM_RIGHTS (§8, §10, §15, §16). init parses no hostile IPC and exits.

## Kernel interfaces reachable BEFORE the sandbox (startup only)

The bootstrap and the early runtime adopt/verify phase touch the kernel
through a small, documented set:

| Interface | Purpose | Process |
|-----------|---------|---------|
| `open("/dev/uinput", O_WRONLY)` | obtain the uinput device | init |
| `ioctl(UI_SET_EVBIT/UI_SET_KEYBIT)` | configure exactly EV_SYN + EV_KEY and the explicit capability set | init |
| `ioctl(UI_DEV_SETUP)` | set the stable device identity/name | init |
| `ioctl(UI_DEV_CREATE)` | register the single virtual keyboard | init |
| `ioctl(UI_GET_SYSNAME)` | read back `input<N>` for verification | init, serve (pre-freeze) |
| sysfs reads (`/sys/devices/virtual/input/input<N>/...`) | verify identity + capability bitmap | init, serve (pre-freeze) |
| `accept4`, `socket`, `bind`, `listen`, `getsockopt(SO_PEERCRED)` | AF_UNIX listener + peer authorization | serve (pre-freeze) |
| `setgroups`/`setgid`/`setuid`, `prctl(PR_CAPBSET_DROP)`, `capset` | drop identity, bounding set and capabilities | supervisor child, serve |
| `prctl(PR_SET_NO_NEW_PRIVS)` | block execve-based privilege gain | serve |
| `prctl(PR_SET_SECCOMP, ...)` | install the seccomp filter | serve |

This phase performs no hostile work: init has no client input, and serve's
pre-freeze steps run before the listener accepts anything (§41).

## Kernel interfaces reachable AFTER the sandbox (runtime)

After the freeze, the runtime can reach exactly:

* `write(2)` on the **one pre-created uinput fd** — 24-byte `input_event`
  images of validated `EV_KEY` down/up/repeat plus the terminating
  `SYN_REPORT` (§18, §19). No other event class is constructible.
* `read`/`write` on **accepted AF_UNIX client sockets**.
* `getsockopt(SO_PEERCRED)` on accepted client sockets (authorization only).
* `poll(2)`/`ppoll(2)`, clock/time, and the memory management required by the
  Rust runtime (see the seccomp allowlist).

Nothing else. In particular, at runtime the broker **cannot**:

* open or reopen any file or device (`open`/`openat`/`openat2`/`creat` are
  not in the allowlist) — including `/dev/uinput`, `/dev/input/event*`,
  `/dev/mem`, `/dev/kmem`, `/dev/kvm`, block devices, procfs/sysfs control
  files (§35, §60);
* issue any `ioctl` (§14, §61) — uinput configuration is impossible;
* create any socket of any family (§31, §62) — AF_UNIX sockets were created
  before the freeze; AF_INET/AF_INET6/AF_PACKET probes return EPERM;
* `execve` — the allowlist has no exec family, so the process cannot run
  another binary at all;
* reach the §33 high-risk subsystems (bpf, perf, ptrace, mount, module
  loading, namespaces, keyctl, userfaultfd, io_uring, reboot, kexec, ...).

## Syscalls reachable after the freeze (seccomp allowlist)

The filter is architecture-aware (x86_64 and aarch64; fail-closed on any
other architecture, §34). The allowlists are defined in
`crates/ferrokeyd/src/sandbox.rs` with per-syscall comments; the unit tests
and the `sandbox-probe` enforcement probes prove the DENIED side
(`ioctl`, `socket`, `openat` on dangerous paths, §92).

## File descriptors reachable at runtime

The FD inventory check (§37) requires exactly: `0,1,2` (stdio), the uinput
device fd, and the listener fd — plus the accepted client fds. The court
reads `/proc/<pid>/fd` and fails on any unexpected target.

## Protocol operations

The FK01 v2 binary protocol (§109) carries only:

* `HELLO` (version + client name), `OPEN_SESSION` (logical session state
  only — **never** kernel device creation, §9),
* `KEY_DOWN` / `KEY_UP` / `KEY_REPEAT` (validated physical key codes,
  §18-§21),
* `RELEASE_ALL` (fail-safe, §23),
* `PING`/`PONG` (liveness).

There is no `CREATE_KEYBOARD`, no text insertion, no macro/exec/URL command
(§48), no arbitrary device naming (§49), and no generic event emission (§19).

## Threat model questions

### Q: If the UI is fully compromised, what kernel-facing authority does the attacker gain?

They may request validated keyboard events through the rate-limited,
authenticated AF_UNIX protocol. They cannot configure uinput, create another
kernel input device, open kernel devices, read physical input, use arbitrary
ioctl, create network sockets, invoke blocked kernel subsystems, gain root,
or gain Linux capabilities (§111).

### Q: If the sandboxed broker itself is compromised, what remains?

They are still non-root with zero capabilities, NO_NEW_PRIVS and seccomp
active; they cannot open new devices, reconfigure uinput, use the network,
read physical keyboards, load modules, mount filesystems, or invoke
bpf/perf/ptrace. Their meaningful authority is bounded to existing FDs,
especially the pre-created virtual keyboard (§112).

## Known limitations

* **Injection power is not kernel exploitation**: a compromised client that
  can type keys can still attempt desktop automation (keyboard injection).
  Session authorization (§27, §28, §114) bounds who may type; it cannot
  prevent a compromised authorized session from typing (§113).
* **Keyboard injection can trigger Ctrl-Alt-Del**: the kernel's VT keyboard
  handler treats Ctrl+Alt+Backspace as a reboot request (SIGINT to PID 1 →
  systemd reboot.target). An authorized-but-hostile client can therefore
  request a system reboot by typing that chord — this is an inherent
  property of *any* keyboard-injection tool (including physical keyboards),
  not a broker defect. The broker's job is to bound *who* may inject;
  preventing specific chords is a UI/policy concern, not the broker's
  (§113). The soak court excludes the chord from its random valid traffic
  for exactly this reason; see `testing/courts/soak/driver.py`.
* **uinput is a kernel API**: Ferrokey cannot guarantee that the Linux kernel
  has no vulnerability reachable by a userspace `write(2)` to a uinput fd.
  The engineering claim is narrower: the UI cannot expand its kernel-facing
  authority beyond the single pre-established virtual-keyboard event channel
  (§115).
* **SysRq in guest X stacks**: injecting KEY_SYSRQ into a guest X server can
  trigger a transient pointer-grab artifact in the *guest compositor*
  (proven a guest-stack property; Ferrokey's kernel-level delivery is
  verified by the courts via evtest). See `testing/courts/full-desktop/`.
* **Session binding is architected, not implemented**: SO_PEERCRED binds the
  broker to a UID/GID (§27), not to a logind session/seat. The long-term
  design binds each broker instance to its active graphical login session
  (§28) so that *this instance serves this session*. Full logind
  integration was not implemented in Phase 3 because the runtime broker's
  seccomp allowlist forbids the `openat`/dbus surface it would need, and the
  §96 release gates do not require it; the design is modular
  (`allowed_uids`/`allowed_gids` is the plug point) and testable when
  implemented. The lock-screen policy is defined in
  `docs/security-architecture.md`; the session-lifetime court (§99)
  applies once session binding exists.
* **Same-UID processes** are not distinguished by SO_PEERCRED alone: any
  process running as an allowed UID may type. This is the acknowledged
  baseline (§114); session/seat binding closes the gap.
