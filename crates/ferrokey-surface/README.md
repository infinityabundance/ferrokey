# ferrokey-surface

Ferrokey's window-system integration — and the piece that makes the project
interesting: **Slint renders and hit-tests; Ferrokey owns the window
semantics.**

The crate implements a custom Slint platform (`slint::platform::Platform` and
`WindowAdapter`) on top of raw Wayland and X11 surfaces, so the exact same
`.slint` UI works on every backend:

```text
                    Slint
                     ▲
                     │ WindowEvent / Renderer
            FerrokeyWindowAdapter
              /                \
             /                  \
     Wayland layer-shell    X11 WM_HINTS.input=false
  keyboard_interactivity=none   (+ dock/above/skip-taskbar)
```

Backends are selected by **capability detection at runtime** — never by
`if compositor == "sway"`-style name matching.

## Features

- `wayland` (default) — `zwlr_layer_shell_v1` overlay with
  `keyboard_interactivity = none`: the compositor guarantees the OSK never
  takes keyboard focus.
- `x11` (default) — ICCCM `WM_HINTS.input = False` window plus
  `override_redirect`, so no WM focus policy can hand focus to the OSK.
- Correct rendering at every visual depth (16/24/32) with byte-stride frames.

## License

Apache-2.0 OR MIT (see the workspace root `LICENSE-APACHE` / `LICENSE-MIT`).
