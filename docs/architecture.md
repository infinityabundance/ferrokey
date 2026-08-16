# Ferrokey architecture

*Phase 4 Workstream 1 — the engineering reference. Read the implementation;
this document describes the repository that actually exists. Where this and
older prose disagree, the code is authoritative (see `docs/architecture.mmd`
for the crate-level diagram and `docs/sequence/` for the interaction
sequences).*

Ferrokey is a **physical on-screen keyboard with kernel-level input
injection**: it renders a keyboard UI that never steals keyboard focus, and
delivers the keys the user touches directly to the Linux input core through
one pre-created `/dev/uinput` device owned by a sandboxed broker. It also
embeds a **terminal workspace**: a real PTY driven by the same OSK through a
separate, direct path that never touches the broker.

The workspace is split so that the security boundary (the broker) is small,
the deterministic semantics (keyboard state, layouts) are pure and testable,
and the window integration (surfaces) is replaceable.

---

## 1. Workspace map

| crate | responsibility | security relevance | primary courts/tests |
|---|---|---|---|
| `ferrokey` (root) | the UI binary + umbrella re-exports | UI is fully unprivileged; speaks only the bounded protocol | x11, wayland, xwayland, applications, chromium, firefox, electron, sdl, focus, touch, terminal |
| `ferrokey-core` | pure keyboard semantics: state machine, modifiers, repeat, layouts, compose, **adaptive key geometry** (WS4: bounded touch statistics, constrained hit-region optimizer, deterministic replay) | the semantic layer that decides what keys reach the system; rollover bound, latch/lock, `release_all` | modifiers, repeat, layouts, dead-keys, altgr, crash, text-mode, adaptive-geometry (ADAPT.*), unit tests |
| `ferrokey-layouts` | layout data + loaders (builtin YAML, real xkbcommon) | layout parsing must be bounded (malformed data must not panic/over-allocate) | layouts, dead-keys, altgr, unit tests |
| `ferrokey-protocol` | binary wire protocol UI ↔ broker (length-prefixed frames) | the privilege boundary surface; hostile input resistant | socket-hijack, cross-user, protocol fuzz (`fuzz_decoder`) |
| `ferrokey-surface` | window-system integration + custom Slint platform | focus-preservation invariant; capability-driven backend selection | wayland, xwayland, x11, backend-selection |
| `ferrokey-terminal` | embedded PTY terminal engine: bounded ANSI parser, grid, scrollback, key encoder, child lifecycle | terminal input never crosses the broker; parser is bounded | terminal-workspace (TERM.* incl. TUI), unit tests |
| `ferrokey-uinput` | `/dev/uinput` virtual keyboard + held-key ledger | the only kernel interface; capability set immutable | uinput, kernel-security |
| `ferrokeyd` | the constrained broker: supervisor, bootstrap, runtime sandbox | THE security boundary: non-root, zero caps, NO_NEW_PRIVS, seccomp, FD inventory | kernel-security, session-lifetime, device-lifetime, permissions, systemd, soak, mutation |
| `ferrokey-proofs` | Kani model-checking harnesses over the **production** `ferrokey-core` state machine; never published | none (verification-only, `publish = false`) | kani-proofs (KANI.HELD.001 … KANI.SEQUENCE.001, KANI.MUTATION.001 — run in the `ferrokey-kani` VM) |
| `xtask` | release tooling: `cargo xtask man` renders the troff man pages and validates their examples through the real config parsers | none (host-side tool) | man-drift (MAN.*) |

---

## 2. Crate-by-crate

### 2.1 `ferrokey-core` — the deterministic keyboard engine

- **Responsibility.** Decide, from explicit key intents and an explicit
  clock, which physical key events the system (or the terminal) must see:
  chords, sticky/latched modifiers, locked modifiers, repeat, layers.
- **Owned state.** `KeyboardState` (`state.rs`): `depressed: BTreeSet<PhysicalKey>`
  (physically held keys, unique by construction, rollover-capped),
  `latched`/`locked: ModifierSet`, `caps_lock`/`num_lock: bool`,
  `active_layer: Layer`, `injected_mods: BTreeMap<PhysicalKey, Vec<PhysicalKey>>`
  (modifiers injected on behalf of a held key), `tap_track`
  (per-key `down_at` + `interleaved`). Plus `RepeatEngine` (`repeat.rs`) and
  `ComposeEngine` (`compose.rs`).
- **Inputs.** `InputRequest::{Key(KeyAction), Text(String)}` with
  `KeyAction::{Down, Up, Tap, ReleaseAll}`; every mutating method takes
  `now: Instant` so time is explicit and reproducible.
- **Outputs.** `Vec<KeyEvent>` (`Down`/`Up`) delivered through the
  `KeySink` trait (`key_down`/`key_up`/`key_repeat`/`release_all`).
- **Dependencies.** None beyond std (`thiserror` for errors). The crate is
  `#![forbid(unsafe_code)]` and has zero OS/UI dependencies.
- **Security relevance.** This is the layer that guarantees a held key is
  bounded (`held_count <= max_held_keys`), that `release_all` clears all
  physical holds, and that latch/lock semantics are deterministic. It never
  touches a device; it only produces events.
- **Key types.** `PhysicalKey` (Linux key codes, `CAPABILITY_SET`), the
  `VirtualKey` wrapper (reserved for a future logical-key space),
  `KeySymbol`/`DeadKey`/`KeyDefinition`/`Layout`, `ModifierKind`/
  `ModifierSet` (bitmask over Shift/Ctrl/Alt/AltGr/Meta/Fn).
- **Primary courts/tests.** `testing/courts/{modifiers,repeat,layouts,
  dead-keys,altgr,crash,full-desktop,text-mode}` + the extensive
  `#[cfg(test)]` suites.

### 2.2 `ferrokey-layouts` — layout data and loaders

- **Responsibility.** Produce `ferrokey_core::Layout` objects from built-in
  YAML data (`builtin.rs`) or from the system's real xkb data through
  `libxkbcommon` (`xkb.rs`, the `xkb` feature; `parse_xkb_spec`, `load_system_layout`,
  `XkbKeymap::to_layout`).
- **Owned state.** The embedded builtin layout table + a parsed xkb keymap
  cache (per `XkbKeymap` instance).
- **Inputs.** A layout id (`"us"`, `"de"`) or an xkb spec (`"us(intl)"`,
  `"de@neo"`).
- **Outputs.** `Layout` (per-key `KeyDefinition` with base/shift/altgr/fn
  symbols + dead keys).
- **Security relevance.** Layout parsing is attacker-visible (user config);
  the parser must be bounded and total. `xkb::validate` rejects malformed
  layouts.
- **Primary courts/tests.** layouts, dead-keys, altgr; unit tests incl.
  `xkb_live` round-trips.

### 2.3 `ferrokey-protocol` — the wire protocol

- **Responsibility.** The binary, length-prefixed protocol between the UI
  and the broker: `Opcode`/`Message`/`ErrorCode` (`message.rs`), framing
  (`codec.rs`), the client (`client.rs`) and peer-identity helpers
  (`peer.rs`).
- **Owned state.** Client connection state (per `Client`), nothing global.
- **Inputs.** Outbound `Message`s from the UI; inbound frames from the
  broker.
- **Outputs.** The wire bytes; decoded `Message`s.
- **Security relevance.** This is the **privilege boundary surface**: the
  broker parses hostile input. The decoder is fuzzed (`fuzz_decoder`,
  `crates/ferrokey-protocol/fuzz`) and the rate/ownership behavior is
  court-tested (socket-hijack, cross-user).
- **Primary courts/tests.** socket-hijack, cross-user, kernel-security
  (SEC.PROTOCOL.*), the `fuzz` harness, unit tests.

### 2.4 `ferrokey-surface` — window-system integration

- **Responsibility.** Backend selection and the custom Slint platform
  adapter: `detect.rs` (capability-driven selection, `SurfaceBackend`),
  `wayland/` (`WaylandSurface` on `zwlr_layer_shell_v1` with
  `keyboard_interactivity = none`), `x11/` (`X11Surface` with
  `WM_HINTS.input = False`), `fallback.rs` (`NullSurface`),
  `slint_adapter.rs` (`PlatformHandle`), `touch.rs`.
- **Owned state.** The backend's surface/window state; the detection result.
- **Inputs.** Session environment (`WAYLAND_DISPLAY`, `DISPLAY`,
  `XDG_RUNTIME_DIR`), compositor globals, pointer/touch events.
- **Outputs.** A `SurfaceBackend` selection + the window adapter that Slint
  renders into; the guarantee (or explicit degraded warning) about focus
  preservation.
- **Security relevance.** The focus-preservation invariant: the OSK must
  never steal keyboard focus. Only `wayland-layer-shell` and `x11-no-input`
  preserve it; degraded modes are explicit.
- **Primary courts/tests.** wayland, xwayland, x11, backend-selection
  (the §65/§66 selection matrix), focus, applications; unit tests in
  `detect.rs`.

### 2.5 `ferrokey-terminal` — the embedded terminal workspace

- **Responsibility.** A real PTY terminal: `pty.rs` (`PtyPair`/`Winsize`),
  `child.rs` (`ChildHandle`/`ShellConfig`/`ChildExit`), `parser.rs`
  (bounded ANSI parser, `limits` in `lib.rs`), `grid.rs`/`scrollback.rs`/
  `viewport.rs`/`selection.rs`/`render.rs`/`modes.rs`, `key_encoder.rs`
  (`TerminalKeyEncoder`: `PhysicalKey` → exact PTY bytes), `sink.rs`
  (`TerminalKeySink` implements `ferrokey_core::KeySink`; `PtySink`
  buffers bytes for the PTY), `clipboard.rs`, `syscall.rs` (the isolated
  `unsafe` module, §82), `terminal.rs` (`Terminal`, the engine; the app
  imports it as `TerminalEngine`).
- **Owned state.** The grid, scrollback, viewport, selection, modes,
  the PTY master fd, the child pid/exit state.
- **Inputs.** Bytes from the PTY master; OSK key actions through
  `TerminalKeySink`; explicit `TerminalConfig`.
- **Outputs.** Encoded key bytes to the PTY (via `PtySink`'s buffer, drained
  in the app's poll cycle); rendered grid updates to the UI.
- **Security relevance.** Terminal input is a **direct path** — OSK →
  encoder → PTY — and never crosses the broker. The ANSI parser is bounded
  (no unbounded allocation), and the only `unsafe` is isolated and audited.
- **Primary courts/tests.** terminal-workspace (TERM.PTY/KEYS/CTRL/ALT/NAV/
  RESIZE/SCROLLBACK/VIEWPORT/ALTSCREEN/SELECTION/IDENTITY/SHELL/NO_UINPUT/
  SECURITY/RESTART/TUI.001–004 with vim/less/htop/tmux), unit tests.

### 2.6 `ferrokey-uinput` — the kernel device

- **Responsibility.** The `/dev/uinput` virtual keyboard: `device.rs`
  (create/verify/emit), `emit.rs` (event writing), `ledger.rs` (held-key
  accounting), `capabilities.rs` (the immutable capability set),
  `ffi.rs` (the `unsafe` uinput ioctls).
- **Owned state.** The device fd + the held-key ledger.
- **Inputs.** `PhysicalKey` events through the `KeySink` impl used by the
  broker.
- **Outputs.** `EV_KEY`/`EV_SYN` events to the kernel input core.
- **Security relevance.** The single kernel interface: exactly one device,
  an immutable capability set, no runtime ioctls after freeze.
- **Primary courts/tests.** uinput, kernel-security (SEC.UINPUT.*), device-lifetime.

### 2.7 `ferrokeyd` — the constrained broker

- **Responsibility.** Own one pre-created uinput keyboard and serve
  authenticated clients over an AF_UNIX socket:
  - `main.rs`: subcommands `start` (supervisor), `init` (bootstrap),
    `serve` (runtime), `sandbox-probe`, `security-status`.
  - `config.rs`: root-owned security-boundary config (UID/GID whitelist,
    optional `session_scope`, rate limits, `max_held_keys`, service
    identity, socket path/mode).
  - `bootstrap.rs`: parses config, spawns `init`, then `serve` with dropped
    identity.
  - `init.rs`/`device.rs`: open `/dev/uinput`, configure the exact
    capability set, `UI_DEV_CREATE`, verify identity/bitmap from sysfs,
    transfer the fd via SCM_RIGHTS, exit.
  - `serve.rs`: adopt the fd, bind the socket, drop groups/gid/uid, capset
    zero, NO_NEW_PRIVS, seccomp freeze, FD inventory, then serve:
    `authorize()` via `SO_PEERCRED` (+ optional session-scope binding),
    rate limiting (`rate_limit.rs`), held-key ledger, disconnect cleanup.
  - `session_scope.rs`: the optional logind session-scope gate (peer cgroup
    lookup through one pre-opened `/proc` dirfd).
  - `sandbox.rs`: the seccomp BPF (arch dispatch, allowlists, optional
    session-gate `openat` block) + enforcement probes.
  - `security.rs`: pre-freeze verification, `verify_before_freeze`, the FD
    inventory (`fds.rs`), NO_NEW_PRIVS.
  - `phase.rs`: the broker lifecycle state machine; `signals.rs`:
    async-signal-safe handling; `server.rs`: the client loop.
- **Owned state.** The device fd, the listener fd, accepted client fds, the
  held-key ledger, phase state.
- **Inputs.** Root-owned config; client IPC frames.
- **Outputs.** Kernel key events on the transferred fd; protocol replies.
- **Security relevance.** Everything after the freeze is enforced, not
  asserted: non-root, zero caps, NNP, seccomp allowlist (+ optional session
  gate), FD inventory, rate limits, per-connection ownership. See
  `docs/threat-model.md`.
- **Primary courts/tests.** kernel-security, session-lifetime, uinput,
  cross-user, socket-hijack, device-lifetime, permissions, systemd, soak,
  kernel-debug (KASAN+UBSAN+LOCKDEP), the §93 mutation courts.

---

## 3. The two input paths

Ferrokey has exactly two key-delivery paths, split at a single routing
decision (`src/input.rs`).

### 3.1 System-input path (broker-mediated)

```text
UI interaction (pointer/touch on the OSK)
        ↓  src/pointer.rs → KeyAction
ferrokey-core KeyboardDriver  (semantics: modifiers, latch/lock, repeat)
        ↓  KeyEvent (Down/Up/Repeat)
InputRouter (Destination::System)
        ↓  src/daemon.rs DaemonLink (KeySink)
ferrokey-protocol client (frames: HELLO/OPEN_SESSION/KEY_DOWN/KEY_UP/RELEASE)
        ↓  AF_UNIX socket
ferrokeyd serve: authorize (SO_PEERCRED + optional session scope) →
        rate-limit → held-key ledger
        ↓  write EV_KEY/EV_SYN on the transferred fd
ferrokey-uinput device
        ↓
Linux input core → focused application
```

(`docs/sequence/system-input.mmd`)

Semantic layers in the current implementation:

- `PhysicalKey` — a concrete Linux key (the only key space used today;
  `VirtualKey::Physical` is the wrapper reserved for a logical space).
- `KeySymbol`/`KeyDefinition`/`Layout` — the per-key symbols per layer
  (base/shift/altgr/fn) and dead keys (`ferrokey-core::layout`).
- `TextSymbol` — the composed/typed text result of the text path
  (`Text(String)` via `InputRequest::Text`; compose engine; never silently
  substituted with clipboard paste).
- Modifier state — `ModifierSet` (held ∪ latched ∪ locked) with
  `Layer::from_modifiers` precedence Fn > AltGr > Shift > Base.
- Repeat state — `RepeatEngine` (`repeat.rs`): delay + cadence, emits
  repeats only for held repeatable keys.
- Held-key state — `KeyboardState::depressed` (rollover-capped set) plus
  the broker-side ledger (`ferrokey-uinput::ledger`).
- Touch intent — **adaptive key geometry** (`ferrokey-core::geometry`, WS4):
  the OSK hit-tests touches against adaptive **hit ellipses** while the
  visible keyboard stays stable. `VisualGeometry` (the drawn rects) is
  separate from `HitGeometry` (the effective touch targets). Per-key
  bounded Welford statistics feed a constrained optimizer (minimize
  expected input cost subject to hard invariants: max center displacement,
  max expansion, min accessible area, neighbor overlap); neighbor
  boundaries are resolved by a deterministic normalized-distance
  competition rule. The optimizer runs off the touch hot path; freeze/
  reset/explain are exposed as controller API + `adaptive` config.
  Evidence rule: only unambiguous hits (confidence below
  `adaptive.evidence_confidence`) are recorded as training samples.

### 3.2 Terminal-input path (direct, broker-free)

```text
UI interaction (OSK in terminal mode)
        ↓  KeyAction
ferrokey-core KeyboardDriver  (same semantics)
        ↓  KeyEvent
InputRouter (Destination::Terminal)
        ↓  src/main.rs wiring
ferrokey-terminal TerminalKeySink (a KeySink over encoder + modes + PtySink)
        ↓  TerminalKeyEncoder: PhysicalKey/LogicalKey → exact PTY bytes
        ↓  PtySink buffer
PTY master (ferrokey-terminal pty.rs)
        ↓
child process (shell, vim, htop, …)
```

Terminal mode is deliberately **not broker-mediated**: no `ferrokeyd`, no
`/dev/uinput`, no compositor focus involvement. The same `KeyboardDriver`
produces the events; the destination decides where they go.
(`docs/sequence/terminal-input.mmd`)

**Shell-aware rows (WS5):** the shortcut row above the terminal keyboard is
context-sensitive. The model lives in `ferrokey-terminal::shell`:
`ShellContext` tracks the interactive shell (`ShellKind`: bash/zsh/fish/
nushell/unknown) and how it was learned (`ShellIdentitySource`): the
initial child is **known from the spawn** (§5.2); later transitions
(shell → vim/htop/tmux/nested shell) come from bounded inspection of the
local `/proc` process tree the terminal owns (§5.3). Rows are pure
keyboard semantics — button → key/chord sequence → `TerminalKeyEncoder` →
PTY bytes, never a hidden shell command (§5.5). `Unknown`/ssh always fall
back to the generic row (§5.4, §5.12); tmux exposes `Ctrl+B` prefix
key-sequences (§5.11); row switching is presentation-only — no keys are
released/pressed, no modes reset, no resize, no child restart (§5.10).
Exact PTY-byte fixtures for every row action are pinned by the SHELL.*
courts (§5.14).

### 3.3 Destination switching

Switching is explicit and safe: the driver is `emergency_release`d first
(held keys + repeat state cleared on the old destination), then the
`InputRouter`'s active destination flips. A key-down never crosses
destinations.

---

## 4. Keyboard-state architecture

The implemented model (`ferrokey-core/src/state.rs`), with the states that
exist in code:

```text
key up ──press──▶ key down (depressed) ──release──▶ key up
                   │   │
                   │   └─ repeatable held key ──tick──▶ RepeatEngine emits repeat
                   ▼
             modifier held ──tap release──▶ latched (sticky)
                   │                          │
                   │                          └─ next qualifying key press ──▶ consumed
                   │                          └─ tap on the active modifier ──▶ disengaged
                   │
                   └─ Caps Lock key tap──▶ locked (Caps Lock mirror = locked SHIFT)

release_all ──▶ every depressed key Up (non-modifiers first, modifiers last);
                injected modifiers released; latched cleared; LOCKED persists
                (Caps/Num Lock are logical state, like physical LEDs)
```

Modifier semantics are click-to-toggle: a quick tap latches an idle modifier
for the next qualifying key, and a tap on an already-active (latched or
locked) modifier disengages it. Modifier taps never invent lock state — the
only lock entry is the Caps Lock key itself.

`Layer::from_modifiers(ModifierSet)` resolves the active layer with
precedence Fn > AltGr > Shift > Base. `KeyEvent` is exactly `Down`/`Up`
(no other event kinds). `press` is total: a duplicate press of an
already-depressed key is a no-op, and a press at `max_held_keys` returns
`Err(Rollover)` without mutating state.

The human-readable companion to the `docs/sequence/keyboard-state.mmd`
diagram and the Kani proofs (`proofs/`, Phase 4 Workstream 3). Adaptive
geometry (Phase 4 Workstream 4) is sketched in
`docs/sequence/adaptive-input.mmd`.

---

## 5. Startup and shutdown

### 5.1 Startup (`docs/sequence/startup.mmd`)

```text
systemd ──▶ ferrokeyd start (supervisor: parses root-owned config)
             ├── spawn init: open /dev/uinput, configure capability set,
             │              UI_DEV_CREATE, verify identity+bitmap, transfer
             │              fd via SCM_RIGHTS, exit
             └── spawn serve: adopt fd, bind AF_UNIX socket, drop to
                              ferrokeyd uid/gid, capset(0), NO_NEW_PRIVS,
                              seccomp freeze, FD inventory, accept clients
ferrokey (UI) ──▶ detect() → SurfaceBackend → surface connect
             ──▶ DaemonLink.poll_connect() → HELLO/OPEN_SESSION handshake
             ──▶ (terminal mode) Terminal::spawn → PTY + child shell
```

### 5.2 Shutdown (`docs/sequence/shutdown.mmd`)

The app's drop path calls `term.borrow_mut().shutdown()` (terminal child
SIGHUP/grace), `release_all` on the driver (no stuck keys), and the daemon
link drains. The broker's `phase.rs` transitions to terminating; the kernel
unregisters the uinput device when the fd closes (proven by
`SEC.STATE.SIGKILL`).

---

## 6. Evidence-linked documentation

| Claim | Implementation | Tests / court | Proof |
|---|---|---|---|
| Rollover is bounded: `held_count <= max_held_keys` | `KeyboardState::press` returns `Err(Rollover)` at the cap; `depressed` is a fixed-capacity set | kernel-security, uinput, soak; unit tests | proved: `KANI.ROLLOVER.001` (WS3 receipt) |
| `release_all` clears every physical hold and latched state; locks persist | `KeyboardState::release_all` | crash, device-lifetime (`SEC.STATE.SIGKILL`) | proved: `KANI.RELEASEALL.001` (WS3 receipt) |
| A held key exists exactly once | `depressed: KeySet` (fixed-capacity sorted set); duplicate `press` is a no-op | soak (chords), unit tests | proved: `KANI.HELD.001` (WS3 receipt) |
| Latch is consumed by the next qualifying key press | `press` clears `latched` after injecting modifiers | modifiers, altgr | proved: `KANI.LATCH.001` (WS3 receipt) |
| Lock only via the Caps Lock key; modifier taps never invent lock state and disengage an active modifier | `release` clears a tap on an active (latched/locked) modifier and latches an idle one; `CapsLock` release toggles the lock | modifiers, altgr | proved: `KANI.LOCK.001` (WS3 receipt) |
| Repeat never manufactures a held key | `RepeatEngine` fixed-capacity table; `tick` emits only tracked keys | repeat; unit tests | proved: `KANI.REPEAT.001` (WS3 receipt) |
| Release of an unheld key cannot create state | `KeyboardState::release` early-returns on absent keys | repeat; unit tests | proved: `KANI.RELEASE.001` (WS3 receipt) |
| Interleaved press/release/`release_all` sequences preserve the invariant set | `KeyboardState` ops over the fixed-capacity tables | soak (chords); unit tests | proved: `KANI.SEQUENCE.001` (WS3 receipt) |
| The proofs detect the regressions they claim to guard | `proofs/run-negative-controls.sh` (mutated copies) | — | proved: `KANI.MUTATION.001` (WS3 receipt) |
| Adaptive geometry never violates the hard invariants | `GeometryConstraints::violated_by` + `AdaptiveGeometry::clamp_to_constraints` | adaptive-geometry (ADAPT.BOUNDS.001) | proved: `ADAPT.BOUNDS.001` (WS4 court) |
| Adaptation is deterministic (same baseline + dataset + version ⇒ same output) | pure f64 pipeline, fixed-seed populations | adaptive-geometry (ADAPT.REPLAY.001, ADAPT.NEIGHBOR.001) | proved: `ADAPT.REPLAY.001` (WS4 court) |
| Freeze/reset are exact user controls | `AdaptiveGeometry::set_frozen` / `reset` | adaptive-geometry (ADAPT.FREEZE.001, ADAPT.RESET.001) | proved: `ADAPT.FREEZE.001`, `ADAPT.RESET.001` (WS4 court) |
| Adaptive hit regions measurably reduce miss rate where improvement is possible | `AdaptiveGeometry::evaluate` over 10 synthetic populations | adaptive-geometry (ADAPT.METRIC.001) | proved: `ADAPT.METRIC.001` (WS4 court, metric report) |
| Shell identity is known from the spawn; transitions come from owned process evidence | `ShellContext::from_spawned_shell` / `inspect` | shell-rows (SHELL.BASH.001, SHELL.NESTED.001, SHELL.TMUX.001, SHELL.SSH.001) | proved: `SHELL.*` (WS5 court) |
| Shell rows are keyboard semantics with exact PTY bytes | `ShellRowKey` sequences → `TerminalKeyEncoder` | shell-rows (SHELL.BYTES.001, SHELL.PTY.001 — real bash in a real PTY) | proved: `SHELL.BYTES.001`, `SHELL.PTY.001` (WS5 court) |
| Row switching is presentation-only (no input-state corruption) | row sequences through the real state machine leave zero held keys | shell-rows (SHELL.STATE.001) | proved: `SHELL.STATE.001` (WS5 court) |
| Broker post-freeze cannot open devices/network/ioctl | `sandbox.rs` filter + probes | kernel-security (SEC.SECCOMP.*, SEC.NET.*), mutation | — |
| Session binding refuses out-of-scope peers | `serve.rs authorize` + `session_scope.rs` | session-lifetime | — |
| Backend selected by capability, not name | `ferrokey-surface::detect::decide` (pure) | backend-selection | — |
| Terminal parser is bounded | `parser.rs` + `limits` | terminal-workspace | — |
| Focus preserved (OSK never steals focus) | layer-shell `keyboard_interactivity=none`; `WM_HINTS.input=False` | wayland, xwayland, x11, focus, applications | — |

---

## 7. Development entrypoints

- Build/test the workspace: `cargo test --workspace` (see
  `CONTRIBUTING.md` for the exact commands).
- Full court pipeline (the single entrypoint, rule 45):
  `bash testing/scripts/run-all-courts.sh` (this runs the docker build and
  unit courts — `testing/courts/build`, `testing/courts/unit` — before the
  VM courts).
- Evidence + receipts live under `testing/evidence/<RUN_ID>/` (gitignored);
  the compatibility receipt and security seal are generated from them
  (`testing/scripts/generate-compat-receipt.sh`,
  `testing/scripts/seal-security-evidence.sh`).
