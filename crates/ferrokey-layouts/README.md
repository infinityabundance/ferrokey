# ferrokey-layouts

Keyboard layout data and loaders.

Layouts are **data files** (`layouts/*.yaml`) — never `.slint` code and never
hard-coded `KEY_X == 'q'` assumptions. Each layout maps a
[`ferrokey_core::key::PhysicalKey`] to its `KeyDefinition`: primary /
shifted / altgr / shift+altgr symbols, an optional Fn-layer symbol and a
repeat policy. `ferrokey-core` decides *what the key means under the active
modifier state*; this crate decides *what symbols exist*.

## Features

- `builtin` (default) — the YAML layouts shipped with the crate (`us`,
  `us-intl`, `gb`, `de`, `fr`, `dvorak`, …), compiled in.
- `xkb` — an xkbcommon bridge for loading system layouts.

## Layout format

```yaml
id: us
name: US
keys:
  - code: KEY_A
    symbols: ["a", "A"]
```

See `layouts/` for the full set.

## License

Apache-2.0 OR MIT (see the workspace root `LICENSE-APACHE` / `LICENSE-MIT`).
