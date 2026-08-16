# Supply-chain review (Phase 3, §83–§85; Phase 5 gate)

Scope: the security boundary — `ferrokeyd` (supervisor + bootstrap + runtime
broker) and `ferrokey-uinput`. The Slint UI is *untrusted* (§2: "assume the
UI is owned"), so its dependency tree is reviewed separately for operational
risk but is not part of the trusted computing base.

Reviewed 2026-08-13; mechanically re-verified 2026-08-16 by the Phase 5
supply-chain gate.

> **The court is authoritative.** `testing/scripts/supply-chain-court.sh`
> runs `cargo deny check` (advisories, licenses, bans, sources) against the
> committed `deny.toml` on every suite run and in CI. It fails on any deny
> — including new unmaintained/unsound crates, unknown licenses, wildcard
> versions, or non-crates.io sources — and it fails on stale policy (an
> allow-list entry or ignore ID that no longer matches the tree). This
> document is a human-readable snapshot; the court is what actually enforces
> it.

## 1. The broker dependency tree is minimal (§83)

`ferrokeyd`'s complete runtime dependency set:

```text
ferrokeyd v0.3.1
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

## 2. Advisories (cargo-deny, live rustsec advisory DB)

The gate's `[advisories]` policy: **every matching advisory is a hard
error**, except the three below, which are ignored with reasons that name
the advisory ID (an ignore ID that no longer matches anything fails the
gate, so the list cannot silently rot). All three are **"unmaintained"**
advisories, all three are reachable **only through the Slint UI**, never
through the broker:

| Advisory | Crate (in tree) | Path | Impact on Ferrokey |
|---|---|---|---|
| RUSTSEC-2024-0436 | paste 1.0.15 | i-slint-compiler → image → exr → pulp → paste | build/UI-time only |
| RUSTSEC-2026-0206 | rustybuzz 0.20.1 | i-slint-common → resvg → usvg → rustybuzz | UI text rendering only |
| RUSTSEC-2026-0192 | ttf-parser 0.25.1 | i-slint-common → resvg → usvg → fontdb → ttf-parser | UI text rendering only |

The four crates in the original 2026-08-13 audit table are now three:
**bincode is no longer in the shipped graph.** The lockfile still records
bincode 2.0.1, but only as a *dev-dependency* of other UI-chain crates'
tests (yoke, chrono, rand, zerotrie, …) — never in the build or runtime
graph — and RUSTSEC-2025-0141 targets bincode 1.x, so it does not match the
pinned version. The 2026-08-13 `cargo audit` snapshot predates the
dependency update that removed it.

`cargo deny check advisories` reports **zero** advisories for `ferrokeyd`,
`ferrokey-uinput`, `ferrokey-core`, `ferrokey-protocol`, or any of their
transitive dependencies. The security boundary is clean.

The three UI-side unmaintained crates are accepted risk with this rationale:

* the UI is explicitly outside the trust boundary — the Phase 3 threat model
  assumes arbitrary code execution in it (§1), so those crates do not add
  attacker capability;
* none of the advisories is a memory-safety CVE; "unmaintained" means
  upstream support risk;
* the slint/rustybuzz/ttf-parser chain is pinned by Slint 1.17.1 and is
  replaced when Slint updates.

## 3. Licensing (cargo-deny license check)

The gate's `[licenses]` policy is a strict allow-list. Under cargo-deny 0.20
(v2) semantics, **every license not listed is denied** — OSI/FSF-approved or
not — and unlicensed crates are denied by default. The allow list contains
exactly the permissive/public-domain licenses used by the current tree
(MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, 0BSD, BSL-1.0,
CC0-1.0, Unlicense, Unicode-3.0, NCSA) — a `deny.toml` edit is required the
moment the tree needs anything else.

Copyleft deliberately has no place in the allow list; it enters only through
**crate-scoped exceptions**, each naming exactly which pinned dependency
carries it:

* **Slint family @1.17.1** (slint, slint-build, slint-macros,
  i-slint-backend-selector, i-slint-common, i-slint-compiler, i-slint-core,
  i-slint-core-macros, i-slint-renderer-software): the crates declare
  `GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR
  LicenseRef-Slint-Software-3.0` — GPL-3.0-only *or* a commercial license.
  The open-source build ships under **GPL-3.0-only**. These crates are
  UI-only and never linked by the broker TCB. The version pins mean a Slint
  upgrade fails the court until the license is re-verified.
  Consequence for distribution: the combined UI binary is GPL-3.0 under the
  open-source option; a commercial Slint license would be required to
  distribute the UI under other terms.
* **r-efi 5.3.0 / 6.0.0**: LGPL-2.1-or-later (the linking-friendly
  copyleft), pulled in by getrandom for its UEFI-target support — compiled
  only on UEFI targets, never on the Linux x86_64 build.

## 4. Duplicate dependencies

`cargo deny check bans` (multiple-versions = warn) reports 7 duplicate
crates, all in the UI's Slint/font chain: font-types (0.11.3/0.12.2),
getrandom (0.3.4/0.4.3), hashbrown (0.14.5/0.16.1/0.17.1), r-efi
(5.3.0/6.0.0), read-fonts (0.39.2/0.41.0), skrifa (0.42.1/0.44.0), syn
(2.0.119/3.0.3). None appears more than once in the broker tree. No
duplicate-crate concern in the TCB.

## 5. Security-critical dependency justification (§85)

| Dependency | Why it is present | Review notes |
|---|---|---|
| `nix` 0.31 | the raw syscall surface of the broker: `poll`, `prctl` (NO_NEW_PRIVS, seccomp, bounding set), `capget/capset`, `setgroups/setgid/setuid`, AF_UNIX sockets, `recvmsg` SCM_RIGHTS fd transfer | type-safe wrappers over libc; the only `unsafe` the broker needs beyond its own isolated modules |
| `libc` (via nix) | errno constants, syscall numbers | pinned by nix; stable ABI |
| `serde` + `serde_yaml` | parsing the **security-boundary config** (§45: root-owned, 0644, parsed only by the brief privileged supervisor) | `serde_yaml` 0.9.34 is the officially deprecated crate (YAML 1.1); no active advisory exists for it (the rustsec DB carries none as of 2026-08-16), but upstream support risk is real. Risk is limited because the config is root-owned and the parser runs only during startup; migration is a tracked follow-up (§7) |
| `thiserror` / `anyhow` | error types for the state machine and CLI | pure macros/derive; no runtime surface |
| `log` | structured daemon logs | facade only; the `env_logger`-style backend is in the crate |
| `tempfile` (dev) | unit tests for socket-path hardening, fd inventory | dev-only; never in the runtime binary |

No dependency grants ambient authority: the runtime broker's seccomp
allowlist (§32) is enforced regardless of what a dependency *could* do.

## 6. Sources

`cargo deny check sources` enforces that **only crates.io** is a valid
source: any git dependency, path-outside-workspace source, or other registry
fails the gate. `wildcards = "deny"` in `[bans]` additionally rejects any
`version = "*"` dependency declaration.

## 7. Receipts

* `cargo tree --edges normal -p ferrokeyd` (above)
* `cargo tree --edges normal -p ferrokey-uinput` (above)
* `cargo deny check` (2026-08-16, cargo-deny 0.20.2, live rustsec DB):
  **advisories ok, bans ok, licenses ok, sources ok** — zero errors, the
  only warnings being the 7 documented UI-only duplicate versions
* unit courts gate `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test` in the disposable Docker builder (never the host)
* the supply-chain court receipt (`SC.SUPPLY.*`) is part of every suite run
  and of CI

## 8. Tracked follow-ups

* migrate `serde_yaml` → maintained fork (`serde_yml`) when the config
  tests are re-validated (upstream is deprecated; no advisory, but support
  risk is tracked);
* re-run the supply-chain gate on every security release and on Slint
  upgrades — the UI's unmaintained transitive crates and the version-pinned
  Slint license exceptions are the first things a bump changes, and both
  fail the court until deliberately re-verified.
