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
- `xkb` — a real `libxkbcommon` bridge: [`xkb::xkbcommon::XkbKeymap`] loads a
  system keymap from rules/layout/variant names (e.g. `us(intl)`, `de@neo`)
  and converts its four XKB levels into a Ferrokey `Layout`. Requires
  `libxkbcommon-dev` at build time; the built-in YAML layouts never do.

  [`xkb::load_system_layout`] accepts the usual XKB specs
  (`"us"`, `"us(intl)"`, `"de@neo"`) and falls back to `None` when the
  keymap cannot be built or fails validation.

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
