//! Event emission for the virtual keyboard.
//!
//! `evdev::VirtualDevice::emit()` appends a `SYN_REPORT` to every batch it is
//! given, so Ferrokey **never** constructs `press; SYN; release; SYN`
//! sequences manually — it hands each batch of `EV_KEY` events to `emit()`
//! and lets the device add synchronization.

use evdev::uinput::VirtualDevice;
use evdev::{EventType, InputEvent, KeyCode};
use std::io;

/// One `EV_KEY` event. The caller batches and lets
/// [`VirtualDevice::emit`] add the trailing `SYN_REPORT`.
pub fn key_event(code: KeyCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::KEY.0 as u16, code.code() as u16, value)
}

/// A press event (`EV_KEY` value=1).
pub fn key_down_event(code: KeyCode) -> InputEvent {
    key_event(code, 1)
}

/// A release event (`EV_KEY` value=0).
pub fn key_up_event(code: KeyCode) -> InputEvent {
    key_event(code, 0)
}

/// Emit a key-down (with automatic `SYN_REPORT`).
pub fn emit_key_down(device: &mut VirtualDevice, code: KeyCode) -> io::Result<()> {
    device.emit(&[key_down_event(code)])
}

/// Emit a key-up (with automatic `SYN_REPORT`).
pub fn emit_key_up(device: &mut VirtualDevice, code: KeyCode) -> io::Result<()> {
    device.emit(&[key_up_event(code)])
}

/// Emit a tap: down then up, in a single batch (single `SYN_REPORT`).
pub fn emit_tap(device: &mut VirtualDevice, code: KeyCode) -> io::Result<()> {
    device.emit(&[key_down_event(code), key_up_event(code)])
}

/// Emit releases for every key in `codes`, batching all `EV_KEY` events into
/// one `SYN_REPORT`-terminated batch. Used by release-all recovery paths.
pub fn emit_release_many(
    device: &mut VirtualDevice,
    codes: impl Iterator<Item = KeyCode>,
) -> io::Result<()> {
    let events: Vec<InputEvent> = codes.map(key_up_event).collect();
    if events.is_empty() {
        return Ok(());
    }
    device.emit(&events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_construction() {
        let down = key_down_event(KeyCode::KEY_A);
        assert_eq!(down.event_type(), EventType::KEY);
        assert_eq!(down.code(), u16::from(KeyCode::KEY_A.code()));
        assert_eq!(down.value(), 1);

        let up = key_up_event(KeyCode::KEY_A);
        assert_eq!(up.value(), 0);
    }
}
