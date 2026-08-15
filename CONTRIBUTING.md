# Contributing to Ferrokey

Practical workflows for people actually changing this repository. The
authoritative architecture reference is `docs/architecture.md`; the threat
model is `docs/threat-model.md`. When in doubt, the code is right and the
prose is wrong — fix the prose.

## Workspace

A single Cargo workspace at the repo root. Members: `ferrokey` (the UI
binary + umbrella library), the library crates under `crates/`
(`ferrokey-core`, `ferrokey-layouts`, `ferrokey-protocol`, `ferrokey-surface`,
`ferrokey-terminal`, `ferrokey-uinput`), and `ferrokeyd` (the broker). The
test targets live in a **separate** workspace at `testing/targets/` (they
are never published and never part of the product build).

```sh
# build + test + lint the whole workspace (this is what the unit court runs)
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# build the release binaries the payload uses
cargo build --release -p ferrokey -p ferrokeyd
```

MSRV is pinned in `rust-toolchain.toml` (Rust 1.96).

## Courts

The single court entrypoint (rule 45) — preflight, docker build/unit/clean
courts, every VM court (X11, browsers, Wayland), the §93 mutation courts,
evidence pull, compatibility receipt, host postflight, and the security
seal — is:

```sh
bash testing/scripts/run-all-courts.sh
```

`RUN_ID` names the run dir (`testing/evidence/<RUN_ID>/`); export one to
make a coherent run:

```sh
RUN_ID=$(date -u +%Y%m%dT%H%M%SZ) bash testing/scripts/run-all-courts.sh
```

Common helpers:

```sh
bash testing/scripts/run-unit-court.sh        # docker build + test + clippy + fmt
bash testing/scripts/run-clean-court.sh       # build from empty caches
bash testing/scripts/run-vm-court.sh <court> x11        # one VM court
bash testing/scripts/run-vm-court.sh <court> wayland    # wayland profile (sway)
bash testing/scripts/run-mutation-courts.sh   # the §93 regression-mutation suite
```

Evidence is the source of truth: the compatibility receipt and security
seal are **generated** from `testing/evidence/<RUN_ID>/`, never hand-edited.

## Resource limits (OOM limits)

The court docker data-root lives on a 26G tmpfs; the suite must fit inside
it and must never let a container drag the host toward its own OOM killer.
Two mechanisms enforce this (`testing/scripts/lib.sh`, "OOM limits").

**Memory** — every heavy court container runs under a hard cap with swap
disabled (`--memory` == `--memory-swap`): a runaway build/proof/VM is OOM-
killed inside its container and the stage fails loudly, while the host keeps
running. The cap is `COURT_MEM_LIMIT` (default `48g`, the proven ceiling for
the Kani verifier):

```sh
COURT_MEM_LIMIT=32g bash testing/scripts/run-all-courts.sh
```

**Disk** — the three largest transient consumers never touch the tmpfs:

- the workspace target + registry (`run_in_builder`, used by the unit,
  adaptive-geometry and shell-aware courts) → `testing/evidence/<RUN_ID>/tmp/`
- the clean-build caches (`run-clean-court.sh`) → same run-scoped dirs
- the VM payload build targets (`run-vm-court.sh`) → same run-scoped dirs

`require_headroom <stage> <min-gib>` gates every heavy stage on data-root
headroom and **aborts before** a stage that would not fit (an ENOSPC
mid-build would corrupt the run). At run start the legacy cache volumes are
dropped, and after the WS3 proofs the kani image is dropped (~2.7G; it is
rebuilt on demand by `bash proofs/run-proofs.sh`). The whole pipeline fits
the 26G tmpfs with margin — no sudo, no daemon config change.

## Conventions

- Receipts: every court assertion goes through `ok`/`bad` (courts run
  inside the VM) or `pass`/`fail` (host-side courts in
  `testing/scripts/lib.sh`). Never `echo PASS` ad hoc.
- `#![forbid(unsafe_code)]` in the library crates; `unsafe` lives only in
  the isolated, audited shims (`ferrokey-terminal/src/syscall.rs`,
  `ferrokey-uinput/src/ffi.rs`, `ferrokeyd/src/sandbox.rs` §82).
- Determinism: keyboard-state methods take an explicit `now: Instant`;
  time-dependent courts use explicit sleeps the helper libs provide.
- Host safety: the host is an orchestrator only. Tests never touch the host
  input subsystem (rules 1, 33, 51).

## Changing the keyboard

### Adding a key

1. Add the variant to `PhysicalKey` in `crates/ferrokey-core/src/key.rs`
   with its Linux code (`linux_code`), name, and place it in
   `CAPABILITY_SET` if the broker must declare it.
2. Add a symbol mapping in the layouts that should show it
   (`crates/ferrokey-layouts/layouts/*.yaml`).
3. Add a key-definition to the view that displays it
   (`src/views.rs`, `KeyDef::with_label`/`chord`).
4. Run `cargo test -p ferrokey-core -p ferrokey-layouts` and the
   `uinput` court (`run-vm-court.sh uinput x11`) — the kernel-security
   court asserts the capability bitmap matches `CAPABILITY_SET`.

### Adding a layout

1. Add a `<id>.yaml` under `crates/ferrokey-layouts/layouts/` (see
   `us.yaml` for the schema: per-key base/shift/altgr/fn symbols and dead
   keys).
2. Register it in `builtin.rs` (`BUILTIN_IDS` + the embedded table).
3. Verify with `cargo test -p ferrokey-layouts` and the `layouts`,
   `dead-keys`, `altgr` courts. System xkb layouts need no registration —
   `load_system_layout` handles `"us(intl)"`/`"de@neo"`-style specs.

### Changing keyboard state (the state machine)

`crates/ferrokey-core/src/state.rs` owns `KeyboardState`. The invariants in
`docs/architecture.md` §6 must keep holding: rollover bound, held-key
uniqueness, `release_all` completeness, latch/lock semantics.

1. Change the semantics in `state.rs` with unit tests for every new
   transition.
2. Run `cargo test -p ferrokey-core`.
3. If you changed an invariant, update the matching Kani proof harness
   (`proofs/`) — a failed proof fails CI (rule: proof failure = CI failure).
4. Run the `modifiers`, `repeat`, `crash`, and `soak` courts.

### Changing terminal key encoding

`crates/ferrokey-terminal/src/key_encoder.rs` maps keys → exact PTY bytes.
Every encoding change must update the byte fixtures the
`terminal-workspace` court uses, and the shell-aware row fixtures
(Workstream 5): button → expected key sequence → exact PTY bytes.

1. Change the encoder + its unit tests.
2. Regenerate/extend the byte fixtures.
3. Run the `terminal-workspace` court.

### Changing surface/backend behavior

`crates/ferrokey-surface/src/detect.rs` is the pure selection policy
(`decide` over `SessionProbe`). Adding a backend means:

1. Add the `SurfaceBackend` variant + the decision row in `decide` with a
   unit test over the new fixture combination.
2. Implement the surface in `wayland/` or `x11/` (or a new module).
3. Extend the `backend-selection` court fixture matrix.

### Changing the broker protocol

`crates/ferrokey-protocol/src/message.rs` defines the wire messages. The
decoder is a privilege boundary:

1. Change `Message`/`Opcode`/`ErrorCode` and both sides (`client.rs`, the
   broker's `server.rs`).
2. Run `cargo test -p ferrokey-protocol` and the decoder fuzz harness:
   `cargo +nightly fuzz run fuzz_decoder` (or the deterministic stress
   tests on stable).
3. Run the `socket-hijack`, `cross-user`, and `kernel-security` courts.

### Changing broker sandbox behavior

`crates/ferrokeyd/src/sandbox.rs` is seccomp; `serve.rs` is the freeze
order. The aarch64 dispatch `jf=0` on the x86_64 JEQ is a pinned invariant
(unit-tested). After any change run `cargo test -p ferrokeyd` and the
`kernel-security` court, then the §93 mutation suite — every mutation must
still be caught on exactly the expected gate.

## Phase-4 addendum workstreams

### Adding a Kani proof

`proofs/` holds the harnesses. Each proof has an id (`KANI.<FAMILY>.<NNN>`),
a harness that invokes production `ferrokey-core` code, an invariant
assertion, and a **negative control** proving the harness catches an
intentional regression.

The verification runs **entirely inside the `ferrokey-kani` container** —
never on the host (rule: no test tooling on the host):

```sh
bash proofs/run-proofs.sh              # all harnesses → proofs/kani-receipt.json
bash proofs/run-negative-controls.sh   # mutated copies must FAIL → proofs/kani-mutation-receipt.json
```

While developing a harness you can target one harness inside the container
(see `run-proofs.sh` for the exact invocation and the OOM guardrails —
`KANI_MEM_LIMIT`, `KANI_HARNESS_TIMEOUT`, and the tmpfs headroom preflight):

```sh
cargo kani -p ferrokey-proofs --harness kani_rollover_held_bound \
    -Z unstable-options --harness-timeout 45m \
    --cbmc-args --unwind 33 --unwinding-assertions
```

Proof conventions:

- Every loop in the verified path must have a **constant trip bound**
  (≤ `MAX_HELD_KEYS` = 32). Iterating a collection through an iterator
  adapter (`.iter().filter().count()`, `for k in set.iter()`, `.any()` over
  a `Vec`) makes CBMC's formula explode — use the flat scan primitives
  (`KeySet::copy_into`, `count_of`, `has_duplicates`, `contains_held`) and
  index-based access instead. `--unwinding-assertions` turns any overlooked
  loop into a loud proof failure rather than silent truncation.
- Symbolic time must sample every region of the tap/latch/lock thresholds
  (the `{0ms, 900ms}` domain) instead of a broad `u64` range — the
  implementation only distinguishes `duration < 400ms` vs `≥`, and
  `< 500ms` vs `≥`, so the small domain is semantically exhaustive.
- Keep the symbolic key universe small (`sequence_key`, `small_key`);
  multi-step symbolic state accumulation is the solver's main cost.

All proofs must pass and must be listed in the machine-readable receipts
(`proofs/kani-receipt.json` and `proofs/kani-mutation-receipt.json`) — the
receipts feed the Phase 4 seal, CI, and the architecture drift court. A
failed proof is a CI failure (no warning-only proof suite).

### Adding an adaptive-geometry invariant

Adaptive geometry lives in `crates/ferrokey-core/src/geometry.rs` (pure,
deterministic, dependency-free) and is consumed by the touch path in
`src/pointer.rs` (hit-test + intended-key evidence) via the `adaptive`
config block. Every constraint (max center displacement, max expansion,
min accessible area, neighbor overlap) is an invariant enforced by
`GeometryConstraints::violated_by` and re-checked by the `ADAPT.BOUNDS.001`
court. Adding or changing behaviour:

1. Change the model in `geometry.rs`; keep it pure and deterministic (the
   fixed-seed `SeededRng` is the only randomness, and only for synthetic
   populations — the pipeline itself has none).
2. Add unit tests in `geometry.rs` (Welford correctness, constraint
   enforcement, freeze/reset exactness, determinism).
3. Add or extend the `ADAPT.*` gate in `crates/ferrokey-core/tests/adapt_courts.rs`
   and re-run the court (inside the builder container, never on the host):

```sh
bash testing/scripts/adaptive-court.sh
```

Deterministic replay: same dataset + same baseline + same optimizer version
⇒ identical output (`ADAPT.REPLAY.001`). New synthetic populations go in
`PopulationKind` (all ten are exercised by `ADAPT.BOUNDS.001` and the
`ADAPT.METRIC.001` report).

### Adding a shell-aware row

Shell rows live in `crates/ferrokey-terminal/src/shell.rs` (the model:
`ShellKind`, `ShellContext`, the `*_ROW` chord tables) and are consumed by
the terminal view's shortcut row in `src/main.rs` + `src/pointer.rs`. Rows
are keyboard semantics, never hidden shell commands: row button →
key/chord sequence → `TerminalKeyEncoder` → PTY bytes (§5.5). Changing a
row or the context model:

1. Edit the row table in `shell.rs`; every action must be a real binding of
   the target shell (never invented — configuration-dependent shortcuts are
   documented as such, and directory-stack-style actions with no default
   bindings are omitted).
2. Add or update the exact byte fixtures in
   `crates/ferrokey-terminal/tests/shell_courts.rs` (SHELL.BYTES.001) and
   the row-semantics gate (SHELL.<SHELL>.002).
3. Run the court (inside the builder container, never on the host):

```sh
bash testing/scripts/shell-court.sh
```

`Unknown` shell identity must always fall back to the generic terminal row
(SHELL.UNKNOWN.001), and row switching must stay presentation-only — the
SHELL.STATE.001 court proves no row sequence leaves held keys or
latched/locked modifiers behind.

## Documentation

- Architecture: `docs/architecture.md` + `docs/architecture.mmd` +
  `docs/sequence/*.mmd`. The drift court
  (`bash testing/scripts/architecture-drift.sh`) fails if documented
  crates/commands/courts/proofs stop existing — update the docs with the
  code.
- Man pages: standard troff sources in `docs/man/` (`ferrokey.1`, `ferrokeyd.1`,
  `ferrokey.yaml.5`, `ferrokeyd.yaml.5`), rendered + example-verified with
  groff (the native man-page toolchain — no extra dependency):

```sh
cargo xtask man
```

  (writes rendered pages to `docs/man/out/`; fails on groff failure).
  The CLI/config drift check is part of the man-page court — a public
  option or config field that is not documented makes the court fail.

## Receipts and release verification

1. Push to `main`; the `courts` workflow runs the full matrix (PRs get the
   shortened soak, `v*` tag pushes get the full 300s soak).
2. Publish in dependency order from `crates/` — crates.io names own their
   version history; bump versions that changed, never reuse a taken
   version:
   `cargo publish -p ferrokey-core -p ferrokey-layouts -p ferrokey-protocol -p ferrokey-surface -p ferrokey-terminal -p ferrokey-uinput -p ferrokeyd -p ferrokey`
3. Tag the release (`vX.Y.Z`); the tag triggers the full release gate.
4. Verify the seal artifacts in `testing/evidence/<RUN_ID>/`:
   `compatibility-receipt.md`, `security-summary.json`,
   `security-receipt.md` — all gates PASS, `HOST_CONTAMINATION: NONE`.
