# ferrokey-uinput

The Linux `/dev/uinput` backend: creating and owning the virtual keyboard
device, emitting `EV_KEY` events, and keeping a defensive held-key ledger so
Ferrokey can never leave a stuck modifier behind.

The device itself is owned by **`ferrokeyd`**, never by the GUI — the UI is
fully unprivileged and talks to the daemon over a Unix socket.

## Features

- **Explicit capability set** — the device advertises exactly the keys
  `ferrokey-core` knows about (`capability_codes`), nothing more.
- **Kernel-correct events** — `key_down` (value=1), `key_up` (value=0) and
  `key_repeat` (value=2, the only form of autorepeat the kernel passes
  through for a held key).
- **Held-key ledger** — tracks every pressed key; `release_all` drains it on
  disconnect/crash so no key is ever left down.
- **Mockable** — `ferrokeyd` swaps in a recording device for tests.

## Example

```rust,no_run
use ferrokey_core::{KeySink, PhysicalKey};
use ferrokey_uinput::{DeviceOptions, VirtualKeyboard};

let mut kb = VirtualKeyboard::new(DeviceOptions {
    name: "ferrokey".into(),
    max_held_keys: 16,
})?;
kb.key_down(PhysicalKey::A)?;
std::thread::sleep(std::time::Duration::from_millis(100));
kb.key_up(PhysicalKey::A)?;
kb.release_all()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Requires access to `/dev/uinput` (root or the `uinput` group).

## License

Apache-2.0 OR MIT (see the workspace root `LICENSE-APACHE` / `LICENSE-MIT`).
