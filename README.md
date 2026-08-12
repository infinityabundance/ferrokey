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
- key events travel over a Unix socket to `ferrokeyd`, the only privileged
  component, which owns a **uinput virtual keyboard**;
- the kernel treats those events exactly like a real keyboard — the focused
  window receives them, and focus never moves.

## How it works

```text
                 ┌──────────────────────────────┐
                 │  ferrokey (unprivileged UI)  │
                 │  Slint + ferrokey-surface    │
                 └──────────────┬───────────────┘
                                │ binary protocol (Unix socket)
                 ┌──────────────▼───────────────┐
                 │  ferrokeyd (privileged)      │
                 │  owns /dev/uinput            │
                 └──────────────┬───────────────┘
                                │ EV_KEY events
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
  repeat, and layouts are a pure state machine — fully unit-testable, no OS
  dependencies.
- **Kernel-correct autorepeat**: repeats are emitted as `EV_KEY` value=2
  events (the kernel filters repeated value=1 presses for held keys), so held
  keys repeat exactly like a real keyboard.
- **Never stuck keys**: `ferrokeyd` releases everything held on disconnect,
  crash, or SIGTERM, and keeps the device alive long enough for the release
  events to reach the compositor.

## Workspace

| package | crates.io | description |
|---|---|---|
| `ferrokey` | [crates.io](https://crates.io/crates/ferrokey) | umbrella crate **and** the `ferrokey` UI binary (the main app) |
| `ferrokey-core` | [crates.io](https://crates.io/crates/ferrokey-core) | keyboard state machine, repeat, layouts, actions |
| `ferrokey-protocol` | [crates.io](https://crates.io/crates/ferrokey-protocol) | binary wire protocol UI ↔ daemon |
| `ferrokey-uinput` | [crates.io](https://crates.io/crates/ferrokey-uinput) | `/dev/uinput` virtual keyboard + held-key ledger |
| `ferrokey-layouts` | [crates.io](https://crates.io/crates/ferrokey-layouts) | layout data files and loaders |
| `ferrokey-surface` | [crates.io](https://crates.io/crates/ferrokey-surface) | Wayland/X11 surfaces + Slint platform adapter |

The `ferrokey` crate is the main application: its `ferrokey` binary is the
UI, and its library is the umbrella re-exporting the five crates. The
daemon (`ferrokeyd`) lives under `crates/`.

## Building

MSRV is pinned in `rust-toolchain.toml` (currently Rust 1.96).

```sh
cargo build --release
cargo test --workspace
```

## Running

```sh
# 1. the daemon (root; owns /dev/uinput). Deny-by-default: configure
#    allowed_uids/allowed_gids first — see testing/fixtures/ferrokeyd.yaml.
sudo ferrokeyd --config testing/fixtures/ferrokeyd.yaml

# 2. the UI (unprivileged).
ferrokey --config testing/fixtures/ferrokey.yaml
```

## Testing: the courts

`cargo test` covers the pure core and protocol layers. Behavioral testing
runs in **QEMU VMs and Docker** against a real guest kernel — see
`testing/` for the court suite (uinput, permissions, focus, crash recovery,
repeat, layouts, applications, wayland/x11/xwayland):

```sh
./testing/scripts/run-vm-court.sh focus x11     # one court
./testing/scripts/run-clean-court.sh            # unit + build courts
```

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
