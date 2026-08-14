# Ferrokey packaging — constrained broker (Phase 3, §38–§40)

This directory ships the production deployment artifacts for the Ferrokey
broker. They are the systemd-layer defense in depth that matches the
**in-process** sandbox (`ferrokeyd serve`): the process that parses hostile
IPC must run non-root with zero capabilities, `NO_NEW_PRIVS`, a seccomp
allowlist, no network, no arbitrary opens and no runtime ioctl.

## Files

| File | Purpose |
|------|---------|
| `ferrokeyd.service` | Hardened systemd unit (see below) |
| `ferrokeyd.yaml` | Production security-boundary config (`/etc/ferrokey/ferrokeyd.yaml`, root-owned 0644, §45) |

## Runtime layout

```
systemd ──▶ ferrokeyd start   (supervisor: parses root-owned config,
                               spawns init, spawns serve, reaps)
                 │
                 ├── ferrokeyd init   (bootstrap: open /dev/uinput, configure
                 │                     the exact immutable capability set,
                 │                     UI_DEV_CREATE one keyboard, verify,
                 │                     transfer fd via SCM_RIGHTS, exit)
                 │
                 └── ferrokeyd serve  (runtime: adopt fd, bind AF_UNIX socket,
                                       drop groups/gid/uid to `ferrokeyd`,
                                       capset(0), NO_NEW_PRIVS, seccomp,
                                       FD inventory, then serve IPC)
```

`ferrokeyd serve` refuses to run as root (§7). The `--allow-root` flag is a
development/testing override only; it is never enabled implicitly and never
used by packaging.

## Session binding and the shipped config (§28, §99)

The production `ferrokeyd.yaml` ships with `session_scope` **unset** —
authorization is UID/GID-based (the documented Phase-3 baseline). Session
binding is implemented and court-proven (`session-lifetime`, `SEC.SESSION.BOUND`),
but `session-N.scope` is assigned dynamically by the session manager at
login, so a static config cannot ship a number. **Dynamic session
discovery** — resolving the active graphical session scope at broker start
(e.g. from the systemd user manager / `loginctl`) so `session_scope` can be
deployed without administrator hard-coding — is an explicit packaging task
for a future phase. Until then, deployments that enable session binding must
supply the correct scope for their session manager, and must treat the
config field as deployment-specific, not static.

## Why the unit looks the way it does

### `PrivateDevices=no` is deliberate (§39)

`PrivateDevices=yes` would create a private `/dev` whose device policy denies
everything — including `/dev/uinput` — and break device creation. The required
property is *"/dev/uinput available exactly where required; all unrelated
devices unavailable"*, which we achieve with:

```ini
PrivateDevices=no
DevicePolicy=closed
DeviceAllow=/dev/uinput rw
```

`DevicePolicy=closed` plus the single allow entry exposes exactly one
character device to the whole process tree (the bootstrap needs it; the
runtime broker cannot reopen it anyway — `open/openat` are absent from the
in-process seccomp allowlist).

### `CapabilityBoundingSet=CAP_SETUID CAP_SETGID CAP_SETPCAP`

The supervisor needs these three capabilities:

* `CAP_SETUID`/`CAP_SETGID` — pre-drop the runtime identity in the `serve`
  child's pre-exec (`setgroups([])` → `setgid` → `setuid`, §3, §41);
* `CAP_SETPCAP` — drop the bounding set itself (`PR_CAPBSET_DROP`) so the
  runtime broker executes with an **empty** bounding set (§58).

The runtime broker's effective/permitted/inheritable/ambient sets are empty
before any client is accepted, verified by `capget` and by the
`SEC.PRIV.*` courts (§5, §58).

### `NoNewPrivileges=yes` (§6)

Blocks execve-based privilege acquisition (setuid/setgid binaries, file
capabilities) for the whole tree, including the supervisor and bootstrap.
`ferrokeyd serve` additionally sets `PR_SET_NO_NEW_PRIVS` in-process and
verifies it before serving.

### `SystemCallFilter` (§31, §33, §38)

```ini
SystemCallArchitectures=native
SystemCallFilter=@system-service
SystemCallFilter=~@keyring userfaultfd name_to_handle_at
SystemCallErrorNumber=EPERM
```

`@system-service` is the recommended allowlist starting point for system
services. On systemd ≥ 261 it already excludes the §33 families (`bpf`,
`perf_event_open`, `ptrace`, mount/namespace/module/kexec/reboot, iopl/ioperm,
io_uring, open_by_handle_at, …). The deny list removes the stragglers that
remain (`@keyring` → `keyctl`/`add_key`/`request_key`; `userfaultfd`;
`name_to_handle_at`). The unit must stay *at least* as permissive as the
supervisor/bootstrap need: `ioctl` (uinput configuration), `open`/`openat`
(config + `/dev/uinput`), `fork`/`execve`/`wait4`, `setuid`/`setgid`/
`setgroups`, and the `AF_UNIX` socket family.

The authoritative runtime freeze remains the in-process seccomp allowlist
installed by `serve` (see `crates/ferrokeyd/src/sandbox.rs`); the unit is
defense in depth covering the brief root phases.

## Verifying the unit

```sh
systemd-analyze verify /etc/systemd/system/ferrokeyd.service
systemd-analyze syscall-filter @system-service   # inspect the allowlist base
```

The kernel-security courts (`testing/courts/kernel-security/`) prove the
**in-process** properties on disposable VMs. A `systemd` court installs this
unit in the guest and asserts the serving process satisfies
`SEC.PRIV.NON_ROOT`, `SEC.PRIV.CAPS_EMPTY` and `SEC.PRIV.NO_NEW_PRIVS`.

## Production config

`/etc/ferrokey/ferrokeyd.yaml` is the security-boundary configuration (§45):
root-owned, 0644, listing the authorized desktop UID (SO_PEERCRED, §27),
`max_connections: 1` (§11), bounded `rate`/`max_held_keys` (§24, §25), the
stable device name (§50) and the service identity (§3). The user-facing
Ferrokey options (layout, theme, text-mode) are *not* broker configuration —
the broker only learns physical key IDs (§46, §47).

## Non-goals

* The human desktop user is **not** granted `/dev/uinput` (§4) — no udev rule,
  no broad `input` group membership. Only the brief root bootstrap touches it.
* The broker never opens `/dev/input/event*` or any physical input device
  (§30).
* No network sockets (§31) — `RestrictAddressFamilies=AF_UNIX` at the unit
  level and no `socket` in the runtime allowlist.
