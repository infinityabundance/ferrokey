# Ferrokey

**An on-screen keyboard that preserves the target application's keyboard
focus — by never taking it in the first place.**

Ferrokey injects key events at the **kernel level** (`/dev/uinput`) instead of
synthesizing X11/Wayland events into a focused window. The keyboard window
never takes keyboard focus, so the application underneath keeps it for the
whole interaction: no focus-grab races, no "click back into the terminal"
dance, no `WM_HINTS.input` hacks on the target.

## Why

Every conventional OSK either steals focus to itself or asks the target to
re-focus after every key. On Wayland the compositor *forbids* foreign
synthetic events, and on X11 focus-grabbing is a well-known source of
flaky, race-prone keyboard state. Ferrokey sidesteps the entire problem:

- the UI is **unprivileged** and renders with Slint;
- key events travel over a tiny binary protocol on a Unix socket to
  `ferrokeyd`, the **constrained broker**: it runs non-root with zero
  capabilities, NO_NEW_PRIVS, a seccomp allowlist, core dumps disabled and
  non-dumpable, and owns one
  pre-created **uinput virtual keyboard** (§ see `docs/security.md`);
- the kernel treats those events exactly like a real keyboard — the focused
  window receives them, and focus never moves.

## How it works

```text
                 ┌──────────────────────────────┐
                 │  ferrokey (unprivileged UI)  │
                 │  Slint + ferrokey-surface    │
                 └──────────────┬───────────────┘
                                │ FK01 v2 binary protocol (AF_UNIX,
                                │ SO_PEERCRED, rate-limited)
                 ┌──────────────▼───────────────┐
                 │  ferrokeyd serve (constrained│
                 │  broker: non-root, zero caps,│
                 │  NO_NEW_PRIVS, seccomp,      │
                 │  no core dumps, non-dumpable)│
                 └──────────────┬───────────────┘
                                │ EV_KEY writes to the ONE pre-created
                                │ virtual keyboard (fd adopted from the
                                │ bootstrap, `ferrokeyd init`)
                 ┌──────────────▼───────────────┐
                 │  Linux input core (uinput)   │
                 └──────────────┬───────────────┘
                                ▼
                 focused application keeps focus
```

- **Surface backends are capability-detected at runtime** — Wayland
  `zwlr_layer_shell_v1` with `keyboard_interactivity = none`, or X11 with
  ICCCM `WM_HINTS.input = False` (plus `override_redirect` so the WM is
  non-participating entirely).
- **Key semantics live in `ferrokey-core`**: modifiers, sticky/locked keys,
  repeat, layouts, and a **dead-key / compose engine** (`' + e → é`,
  `compose o c → ©`) are a pure state machine — fully unit-testable, no OS
  dependencies.
- **Keyboard views are presentation, not semantics**: `compact` (the default
  6-row OSK) and `full` (the complete desktop keyboard: function row, Print
  Screen / Scroll Lock / Pause, navigation cluster, arrows, numeric keypad,
  media and brightness keys) are arrangements over the *same* physical-key
  engine — `ferrokey --view full` switches the whole board without touching
  the key state machine.
- **Kernel-correct autorepeat**: repeats are emitted as `EV_KEY` value=2
  events (the kernel filters repeated value=1 presses for held keys), so held
  keys repeat exactly like a real keyboard.
- **Never stuck keys**: `ferrokeyd` releases everything held on disconnect,
  crash, or SIGTERM, and keeps the device alive long enough for the release
  events to reach the compositor.
- **Touch and pen input**: touchscreens are first-class (XInput2/XI2 on X11,
  `wl_touch` on Wayland) and drive the OSK exactly like a click.
- **System layouts via libxkbcommon**: the `xkb` feature loads real desktop
  keymaps (`us(intl)`, `de@neo`, …) through `ferrokey-layouts`; the built-in
  YAML layouts need no C library.
- **Hostile-input resistance is fuzzed**: the protocol decoder has a
  cargo-fuzz harness (nightly, CI) plus deterministic byte-level stress tests
  on stable.

## Workspace

| package | crates.io | description |
|---|---|---|
| `ferrokey` | [crates.io](https://crates.io/crates/ferrokey) | umbrella crate **and** the `ferrokey` UI binary (the main app) |
| `ferrokey-core` | [crates.io](https://crates.io/crates/ferrokey-core) | keyboard state machine, repeat, layouts, actions |
| `ferrokey-protocol` | [crates.io](https://crates.io/crates/ferrokey-protocol) | binary wire protocol UI ↔ daemon |
| `ferrokey-uinput` | [crates.io](https://crates.io/crates/ferrokey-uinput) | `/dev/uinput` virtual keyboard + held-key ledger |
| `ferrokey-layouts` | [crates.io](https://crates.io/crates/ferrokey-layouts) | layout data files and loaders |
| `ferrokey-surface` | [crates.io](https://crates.io/crates/ferrokey-surface) | Wayland/X11 surfaces + Slint platform adapter |
| `ferrokey-terminal` | [crates.io](https://crates.io/crates/ferrokey-terminal) | embedded PTY terminal engine: bounded ANSI parser, grid, scrollback, key encoder, child-session lifecycle |
| `ferrokeyd` | [crates.io](https://crates.io/crates/ferrokeyd) | the constrained broker: supervisor, bootstrap, runtime sandbox |

The `ferrokey` crate is the main application: its `ferrokey` binary is the
UI, and its library is the umbrella re-exporting `ferrokey-core`, -layouts,
-protocol, -surface and -uinput (`ferrokey-terminal` is a workspace
dependency of the binary, not re-exported). The daemon (`ferrokeyd`) lives
under `crates/`.

## Building

MSRV is pinned in `rust-toolchain.toml` (currently Rust 1.96).

```sh
cargo build --release
cargo test --workspace
```

## Running (Phase 3 security model)

`ferrokeyd` is a **constrained broker**, not a privileged daemon: the runtime
(`serve`) drops to a dedicated unprivileged identity with zero capabilities,
NO_NEW_PRIVS, a seccomp allowlist, core dumps disabled, non-dumpable, no
network and no arbitrary opens before accepting any client. The production
deployment is the hardened systemd unit
(`PACKAGING/ferrokeyd.service`, §38-§40) — the human user is **never** granted
`/dev/uinput` (§4).

```sh
# 1. install the unit + root-owned config (see PACKAGING/README.md)
systemctl enable --now ferrokeyd

# 2. the UI (unprivileged).
ferrokey --config testing/fixtures/ferrokey.yaml
```

The development/testing override (`ferrokeyd serve --allow-root`) is
never enabled implicitly (§7).

## Security documentation

- [`docs/security.md`](docs/security.md) — the security claim, release gates
  and hostile-court verdict (§115, §117);
- [`docs/security-architecture.md`](docs/security-architecture.md) — the
  end-state architecture and runtime authorities (§116);
- [`docs/threat-model.md`](docs/threat-model.md) — the kernel attack-surface
  threat model (§107);
- `PACKAGING/` — the hardened systemd unit, production config and rationale
  (§38-§40, §45).

## Testing: the courts

`cargo test` covers the pure core and protocol layers. Behavioral testing
runs in **QEMU VMs and Docker** against a real guest kernel — the courts
never touch the host input stack (host preflight aborts if a GUI session or
input device is visible).

The full suite is a single entrypoint:

```sh
./testing/scripts/run-all-courts.sh
```

which runs, in order:

- **Docker build courts** — `build.workspace` (build + test + clippy + fmt),
  `core.unit`, and `build.clean` (pristine-cache build);
- **Security VM courts (X11)** — `kernel-security` (SEC.PRIV/SECCOMP/DEVICE/
  NET/UINPUT/PROTOCOL/STATE gates, §55–§97), `systemd` (hardened unit,
  §38–§40), `soak` (long-run bounds, §98), `socket-hijack` (§101),
  `cross-user` (§100), `device-lifetime` (§73);
- **X11 VM courts** (`debian-12`) — `uinput`, `permissions`, `x11`, `focus`,
  `crash`, `repeat`, `modifiers`, `layouts`, `applications` (GTK/Qt/Slint/
  raw-X11 targets), `dead-keys`, `text-mode`, `touch`, `altgr`,
  `full-desktop`, `sdl`, `terminal`;
- **browsers appliance VM courts** (`debian-12-browsers`) — `firefox`,
  `chromium`, `electron`;
- **Wayland VM courts** — `wayland`, `xwayland`;
- **mutation courts (§93)** — six deliberate security regressions, each
  proven caught by the exact gate it breaks;
- with `KASAN=1` additionally the **kernel-debug court** (§66–§68) on a
  KASAN+UBSAN+LOCKDEP kernel (built once by
  `testing/scripts/build-kasan-kernel.sh`);
- evidence collection, the **security evidence seal** (§90, §91, §96), and
  the generated **compatibility receipt** (see below).

Any failed court receipt aborts the suite non-zero (§94): there is no
`|| true` masking, and the guest→oracle→host exit chain propagates.

The high-intensity hostile audit (§97) is one command:

```sh
./testing/scripts/security-court.sh --hostile
```

Run one court at a time with:

```sh
./testing/scripts/run-vm-court.sh <court> x11          # X11 profile
./testing/scripts/run-vm-court.sh <court> wayland      # Wayland profile
./testing/scripts/run-vm-court.sh firefox x11 debian-12-browsers
./testing/scripts/run-clean-court.sh                   # Docker build courts
./testing/scripts/run-mutation-courts.sh               # SEC.COURT.MUTATION
```

### The browsers appliance image

The Firefox/Chromium/Electron courts need a ~1.5 GB browser stack, so it is
baked once (checksum-pinned downloads) into `debian-12-browsers.qcow2` by
`testing/vm/qemu/build-browsers-image.sh` and cached in the `ferrokey-vm-state`
docker volume. It is built on demand on first use; to force a rebuild,
delete the cached `images/debian-12-browsers.qcow2` from that volume.

### Compatibility receipt (§37)

Every court writes a machine-readable `receipt.json` plus a per-assertion
`assertions.json` into the evidence volume. `run-all-courts.sh` then
**generates** the compatibility statement from that evidence:

```sh
./testing/scripts/generate-compat-receipt.sh <run-id>
```

This produces `compatibility-receipt.{json,md}` in the run directory and a
copy at the top of the evidence volume — rows are PASS/FAIL/UNKNOWN only from
actual court receipts, never hand-edited.

The protocol decoder is additionally fuzzed with libFuzzer (nightly):

```sh
cd crates/ferrokey-protocol/fuzz
cargo +nightly fuzz run fuzz_decoder -- -max_total_time=120
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
