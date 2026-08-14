# Ferrokey security architecture (Phase 3)

The end-state architecture. Every property named here is enforced by the
in-process sandbox AND by the systemd unit (`PACKAGING/ferrokeyd.service`),
and proven inside disposable VMs by the security courts
(`testing/courts/kernel-security/`, `testing/courts/systemd/`,
`testing/courts/soak/`).

```text
┌─────────────────────────────────────────────┐
│                 ferrokey                    │
│                                             │
│ Slint UI                                    │
│ layouts                                     │
│ text mode                                   │
│ accessibility                               │
│                                             │
│ ASSUME COMPROMISED                          │
└──────────────────┬──────────────────────────┘
                   │
                   │ AF_UNIX
                   │ fixed bounded protocol (FK01 v2)
                   ▼
┌─────────────────────────────────────────────┐
│           ferrokeyd-runtime (serve)         │
│                                             │
│ dedicated non-root UID (ferrokeyd)          │
│ zero capabilities (incl. empty bounding set)│
│ PR_SET_NO_NEW_PRIVS                         │
│ seccomp (arch-aware allowlist)              │
│ AF_UNIX only                                │
│ no arbitrary open                           │
│ no runtime uinput ioctl                     │
│ no physical input                           │
│ no network                                  │
│ immutable key capability set                │
│ held-key ledger                             │
└──────────────────┬──────────────────────────┘
                   │
                   │ existing FD only
                   │ EV_KEY writes only
                   ▼
          Ferrokey Virtual Keyboard
                   │
                   ▼
            Linux input core


BOOTSTRAP PATH

┌─────────────────────────────────────────────┐
│            ferrokeyd-init                   │
│                                             │
│ no hostile IPC                              │
│ minimal temporary authority                 │
│ open /dev/uinput                            │
│ configure exact key bitmap                  │
│ UI_DEV_CREATE                               │
│ transfer FD (SCM_RIGHTS)                    │
│ exit                                        │
└─────────────────────────────────────────────┘
```

## Process model

* `ferrokeyd start` — supervisor. Runs briefly as root under systemd,
  parses the root-owned security-boundary config (§45), spawns `init`,
  then spawns `serve` with a pre-exec that empties the bounding set
  (`PR_CAPBSET_DROP`), drops supplementary groups, then setgid/setuid to the
  dedicated `ferrokeyd` identity (§3, §58). It then only forwards signals
  and reaps the child.
* `ferrokeyd init` — tiny bootstrap (§16). Opens `/dev/uinput`, configures
  exactly the immutable capability set, creates ONE keyboard, verifies
  identity + bitmap from sysfs, transfers the fd, exits. No client input.
* `ferrokeyd serve` — runtime broker. Adopts the fd, re-verifies it, binds
  the hardened AF_UNIX socket, then freezes (§41):
  `capset(0)` → `NO_NEW_PRIVS` → seccomp → enforcement probes → FD
  inventory → serve. Never root (§7).

## Freeze order (§41)

```text
initialize privileged/narrow resources (init: uinput device)
│
▼
adopt device fd; bind listener
│
▼
verify FD inventory baseline
│
▼
capset(0)                      (§5)
│
▼
PR_SET_NO_NEW_PRIVS            (§6)
│
▼
install seccomp allowlist      (§32-§34)
│
▼
enforcement probes             (§61, §62, §92)
│
▼
final FD inventory             (§37)
│
▼
accept untrusted clients
```

There is no hostile-input window before containment.

## Lifecycle state machine (§42, §88)

```text
Initializing → DeviceConfigured → Sandboxed → Serving → ShuttingDown
```

Illegal transitions (Serving → configure uinput, Serving → regain
capability, Serving → open device) fail; the state machine is unit-tested
exhaustively (`crates/ferrokeyd/src/phase.rs`).

## Runtime authorities (the complete list)

The runtime broker owns, after the freeze:

1. one pre-created virtual keyboard FD (write validated `EV_KEY` events only),
2. one AF_UNIX listening socket,
3. accepted AF_UNIX client FDs,
4. clock/time and memory-management syscalls required by the Rust runtime,
5. logging to stdio.

Nothing else. The FD inventory court checks `/proc/<pid>/fd` and the seccomp
allowlist checks the syscall side; both are enforced, not just asserted
(§92).

## Defense in depth

| Layer | Mechanism |
|-------|-----------|
| 1 (UI) | unprivileged; speaks only the bounded protocol |
| 2 (broker) | non-root, zero caps, NNP, seccomp, FD inventory |
| 3 (bootstrap) | tiny, exits before any client input |
| 4 (systemd) | `NoNewPrivileges`, `CapabilityBoundingSet`, `DevicePolicy=closed` + `DeviceAllow=/dev/uinput rw`, `Protect*`, `RestrictAddressFamilies=AF_UNIX`, `SystemCallArchitectures=native` (§38-§40) |
| 5 (kernel) | uinput is the only kernel interface; capability set immutable |

## What the UI can do (§111)

A compromised UI may request validated keyboard events through the
rate-limited, authenticated AF_UNIX protocol. It cannot configure uinput,
create another kernel input device, open kernel devices, read physical
input, use arbitrary ioctl, create network sockets, invoke blocked kernel
subsystems, gain root, or gain Linux capabilities.

## What a compromised broker can do (§112)

Still non-root with zero capabilities, NO_NEW_PRIVS, seccomp active; cannot
open new devices, reconfigure uinput, use the network, read physical
keyboards, load modules, mount filesystems, or invoke bpf/perf/ptrace. Its
authority is bounded to existing FDs, especially the pre-created virtual
keyboard.

## Surface backend selection (§65/§66)

The UI's surface backend is chosen by **capability detection**, never by
compositor name: `WAYLAND_DISPLAY` + `zwlr_layer_shell_v1` →
`wayland-layer-shell`; Wayland without layer-shell falls back to an X11
surface on `DISPLAY` (`x11-no-input`, XWayland) or to an explicit degraded
mode when no X display exists; a bare X11 session → `x11-no-input`;
headless → `none`. The decision is a pure function of observed facts
(`ferrokey-surface::detect::decide` over `SessionProbe`), every fallback
carries its rejection reason in the reported detail, and the
`backend-selection` court asserts the real app's startup log line across
the five fixture sessions (headless, X11-only, sway layer-shell, the
mini-compositor without layer-shell ± X11). Only `wayland-layer-shell` and
`x11-no-input` preserve the focus invariant (§13); the degraded modes show
explicit warnings.

## Session authorization (§27, §28, §114)

Authorization identity comes from the kernel via `SO_PEERCRED`; the client
never supplies a UID. The active broker binds to the authorized desktop UID
list from the root-owned config. **Session/seat binding** (§28, §99): the
broker may additionally bind to a logind session scope
(`session_scope: session-N.scope` in the root-owned config). A client is
then authorized only if its cgroup contains the same session scope. The
broker's post-freeze peer lookup is a single read-only `openat` relative to
the pre-opened `/proc` directory under a highly constrained syscall shape
(seccomp pins `dirfd` + `O_RDONLY|O_CLOEXEC`; the `"<pid>/cgroup"` path
contract is enforced by `session_scope.rs` code, not by the filter — the
residual is read-only access to other world-readable `/proc` files, with no
write path and no change to injection authority; see
docs/threat-model.md). The `session-lifetime` court proves the in-session
client is served, the out-of-session client is refused, and the sandbox
denials stay intact.

## Lock-screen and session policy (§29, §99)

The Phase 3 policy, defined here so the broker never silently injects into
authentication surfaces:

| State | Policy | Enforcement today |
|---|---|---|
| session unlocked / active | the desktop broker may serve the authorized UID | SO_PEERCRED whitelist (§27) |
| session locked | the **normal desktop broker must not inject into the lock screen** — the virtual keyboard is destroyed when the owning broker stops; the lock screen is a different seat/session, so a UID-scoped broker does not reach it by construction | device lifecycle (§73 court) |
| session inactive / terminated | the broker instance is expected to be stopped by the session manager; the device unregisters with it | `SEC.DEVLIFE.*` + `SEC.STATE.SIGKILL` courts |
| seat ownership changed | a new session/seat starts its own broker; the old broker must be stopped | restart-safe device lifecycle (§73) |

Session binding (logind `session_scope`, §28/§99) is implemented: the
broker matches the peer's cgroup session scope and refuses same-UID peers
outside it. Seat-level binding (logind seat names) is the remaining
hardening; the current design stops the per-session broker when the seat is
released. Lock-screen OSK support, if ever implemented, must be a separate
explicitly audited integration (§29); the normal desktop broker is destroyed
when its session ends, so it cannot reach the lock screen by construction.
