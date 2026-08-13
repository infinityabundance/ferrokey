# Supply-chain review (Phase 3, §83–§85)

Scope: the security boundary — `ferrokeyd` (supervisor + bootstrap + runtime
broker) and `ferrokey-uinput`. The Slint UI is *untrusted* (§2: "assume the
UI is owned"), so its dependency tree is reviewed separately for operational
risk but is not part of the trusted computing base.

Reviewed 2026-08-13.

## 1. The broker dependency tree is minimal (§83)

`ferrokeyd`'s complete runtime dependency set:

```text
ferrokeyd v0.2.0
├── anyhow
├── ferrokey-core            (workspace: PhysicalKey / capability set)
├── ferrokey-protocol        (workspace: FK01 decoder, peer identity)
├── ferrokey-uinput          (workspace: uinput fd, ledger, emission)
├── log
├── nix                       (syscall surface: poll, prctl, capset, sockets)
├── serde
├── serde_yaml               (security-boundary config parsing, §45)
└── thiserror
```

`ferrokey-uinput`'s runtime dependencies: `ferrokey-core`, `log`, `nix`,
`thiserror`.

There is **no** Slint, HTTP, plugin, script, or UI dependency in the broker.
`cargo tree --edges normal` receipts are produced by the build courts.

## 2. `cargo audit` (2026-08-13 advisory DB)

```text
warning: 4 allowed warnings found
```

All four are **"unmaintained crate"** advisories and all four are reachable
**only through the Slint UI**, never through the broker:

| Advisory | Crate | Path | Impact on Ferrokey |
|---|---|---|---|
| RUSTSEC-2025-0141 | bincode (unmaintained) | i-slint-compiler → image → exr → pulp → paste/bincode | build-time / UI only |
| RUSTSEC-2024-0436 | paste (unmaintained) | i-slint-compiler → image → exr → pulp → paste | UI only |
| RUSTSEC-2026-0206 | rustybuzz (unmaintained) | i-slint-common → resvg → usvg → rustybuzz | UI text rendering only |
| RUSTSEC-2026-0192 | ttf-parser (unmaintained) | i-slint-common → resvg → usvg → fontdb → ttf-parser | UI text rendering only |

`cargo audit` reports **zero** advisories for `ferrokeyd`, `ferrokey-uinput`,
`ferrokey-core`, `ferrokey-protocol`, or any of their transitive
dependencies. The security boundary is clean.

The four UI-side unmaintained crates are accepted risk with this rationale:

* the UI is explicitly outside the trust boundary — the Phase 3 threat model
  assumes arbitrary code execution in it (§1), so those crates do not add
  attacker capability;
* none of the advisories is a memory-safety CVE; "unmaintained" means
  upstream support risk;
* the slint/rustybuzz/ttf-parser chain is pinned by Slint 1.17.1 and is
  replaced when Slint updates.

## 3. Duplicate dependencies

`cargo tree -d` reports 43 duplicate versions, all in the UI's Slint/font
chain (font-types, read-fonts, hashbrown, lyon_*, bytemuck, …). None appear
more than once in the broker tree. No duplicate-crate concern in the TCB.

## 4. Security-critical dependency justification (§85)

| Dependency | Why it is present | Review notes |
|---|---|---|
| `nix` 0.31 | the raw syscall surface of the broker: `poll`, `prctl` (NO_NEW_PRIVS, seccomp, bounding set), `capget/capset`, `setgroups/setgid/setuid`, AF_UNIX sockets, `recvmsg` SCM_RIGHTS fd transfer | type-safe wrappers over libc; the only `unsafe` the broker needs beyond its own isolated modules |
| `libc` (via nix) | errno constants, syscall numbers | pinned by nix; stable ABI |
| `serde` + `serde_yaml` | parsing the **security-boundary config** (§45: root-owned, 0644, parsed only by the brief privileged supervisor) | `serde_yaml` 0.9.34 is the officially deprecated crate (YAML 1.1); its replacement (`serde_yml`) is a maintained fork. Risk is limited because the config is root-owned and the parser runs only during startup; migration is a tracked follow-up |
| `thiserror` / `anyhow` | error types for the state machine and CLI | pure macros/derive; no runtime surface |
| `log` | structured daemon logs | facade only; the `env_logger`-style backend is in the crate |
| `tempfile` (dev) | unit tests for socket-path hardening, fd inventory | dev-only; never in the runtime binary |

No dependency grants ambient authority: the runtime broker's seccomp
allowlist (§32) is enforced regardless of what a dependency *could* do.

## 5. Receipts

* `cargo tree --edges normal -p ferrokeyd` (above)
* `cargo tree --edges normal -p ferrokey-uinput` (above)
* `cargo audit` (2026-08-13): 0 advisories reach the broker; 4 UI-only
  unmaintained-crate warnings, accepted with rationale
* `cargo tree -d`: 43 UI-only duplicates, none in the TCB
* unit courts gate `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test` in the disposable Docker builder (never the host)

## 6. Tracked follow-ups

* migrate `serde_yaml` → maintained fork (`serde_yml`) when the config
  tests are re-validated;
* re-run `cargo audit` on every security release and on Slint upgrades
  (the UI's unmaintained transitive crates are the first thing a bump
  changes).
