# ferrokey-core

The pure, deterministic heart of Ferrokey. No OS dependencies, no input
devices, no GUI toolkit: everything here is a function of explicit input and
an explicit clock, which is what makes it fully unit-testable.

The UI speaks [`action::InputRequest`]s; [`action::KeyboardDriver`]
translates them into kernel-level key events via a [`action::KeySink`]
(implemented by `ferrokey-uinput`'s device or by the `ferrokey-protocol`
client that talks to `ferrokeyd`).

## Features

- **Modifiers, sticky and locked keys** — Shift/Ctrl/Alt/Super, tap-to-latch,
  double-tap-to-lock, with modifier injection on the following keypress.
- **Kernel-correct autorepeat** — held keys repeat as `EV_KEY` value=2 events
  through [`KeySink::key_repeat`], not repeated value=1 presses (which the
  kernel filters for held keys).
- **Layouts** — a `Layout` maps physical keys to symbols (primary / shifted /
  altgr / shift+altgr / Fn), with a repeat policy per key.
- **Actions and text input** — `KeyAction::{Down, Up, Tap, ReleaseAll}` plus
  a best-effort text channel that is never silently replaced with clipboard
  paste.

## Example

```rust,no_run
use ferrokey_core::{
    KeyAction, KeySink, KeyboardDriver, PhysicalKey, RepeatSettings, SinkError,
    StateSettings, VirtualKey,
};
use std::sync::Arc;
use std::time::Instant;

// Any sink works: uinput device, protocol client, or a test recorder.
struct Sink;
impl KeySink for Sink {
    fn key_down(&mut self, _key: PhysicalKey) -> Result<(), SinkError> { Ok(()) }
    fn key_up(&mut self, _key: PhysicalKey) -> Result<(), SinkError> { Ok(()) }
    fn release_all(&mut self) -> Result<(), SinkError> { Ok(()) }
}

let mut driver = KeyboardDriver::new(
    StateSettings::default(),
    RepeatSettings::default(),
    Arc::new(ferrokey_core::Layout::empty("us", "US")),
    Box::new(Sink),
);

driver.handle_action(
    KeyAction::Tap,
    VirtualKey::Physical(PhysicalKey::A),
    Instant::now(),
)?;
# Ok::<(), ferrokey_core::DriverError>(())
```

## License

Apache-2.0 OR MIT (see the workspace root `LICENSE-APACHE` / `LICENSE-MIT`).
