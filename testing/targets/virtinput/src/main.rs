//! Court tool: a virtual pointer + keyboard created via uinput **inside the
//! guest VM**.
//!
//! The guest kernel is the only kernel involved (rule 3): this device exists
//! in the VM's input subsystem, so Wayland compositors (weston), Xorg and
//! libinput treat it as a real input device. Commands arrive on stdin:
//!
//! ```text
//! move <dx> <dy>     # relative motion
//! move-abs <x> <y>   # absolute motion (not supported for REL devices)
//! down <button>      # 0=left 1=middle 2=right
//! up <button>
//! click <button>
//! key <code>         # tap a linux key code
//! key-down <code>
//! key-up <code>
//! sleep <ms>
//! ```

use evdev::uinput::VirtualDevice;
use evdev::{
    AttributeSet, BusType, EventType, InputEvent, InputId, KeyCode, RelativeAxisCode,
    RelativeAxisEvent,
};
use std::io::{self, BufRead};
use std::time::Duration;

fn main() -> io::Result<()> {
    let mut builder = VirtualDevice::builder()?;
    builder = builder
        .name("Ferrokey Court Virtual Pointer")
        .input_id(InputId::new(BusType(0x03), 0xFE30, 0xFE31, 0x0001));

    let mut keys = AttributeSet::new();
    keys.insert(KeyCode::BTN_LEFT);
    keys.insert(KeyCode::BTN_MIDDLE);
    keys.insert(KeyCode::BTN_RIGHT);
    // A generous key capability set for court tests (full range is fine
    // inside the disposable VM).
    for code in 1..=0x110 {
        keys.insert(KeyCode::new(code));
    }
    builder = builder.with_keys(&keys)?;

    let mut rel = AttributeSet::new();
    rel.insert(RelativeAxisCode::REL_X);
    rel.insert(RelativeAxisCode::REL_Y);
    rel.insert(RelativeAxisCode::REL_WHEEL);
    builder = builder.with_relative_axes(&rel)?;

    let mut device = builder.build()?;

    println!("virtinput ready: {:?}", device.get_syspath()?);

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts[0] {
            "move" => {
                let dx: i32 = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                let dy: i32 = parts.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
                if dx != 0 {
                    device.emit(&[RelativeAxisEvent::new(RelativeAxisCode::REL_X, dx).into()])?;
                }
                if dy != 0 {
                    device.emit(&[RelativeAxisEvent::new(RelativeAxisCode::REL_Y, dy).into()])?;
                }
            }
            "down" => {
                let btn = button(parts.get(1).copied().unwrap_or("0"));
                device.emit(&[button_event(btn, 1)])?;
            }
            "up" => {
                let btn = button(parts.get(1).copied().unwrap_or("0"));
                device.emit(&[button_event(btn, 0)])?;
            }
            "click" => {
                let btn = button(parts.get(1).copied().unwrap_or("0"));
                device.emit(&[button_event(btn, 1), button_event(btn, 0)])?;
            }
            "key" | "tap" => {
                let code: u16 = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(30);
                device.emit(&[key_event(code, 1), key_event(code, 0)])?;
            }
            "key-down" => {
                let code: u16 = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(30);
                device.emit(&[key_event(code, 1)])?;
            }
            "key-up" => {
                let code: u16 = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(30);
                device.emit(&[key_event(code, 0)])?;
            }
            "sleep" => {
                let ms: u64 = parts.get(1).and_then(|v| v.parse().ok()).unwrap_or(100);
                std::thread::sleep(Duration::from_millis(ms));
            }
            other => {
                eprintln!("unknown command: {other}");
            }
        }
        device.emit(&[InputEvent::new(EventType::SYNCHRONIZATION.0, 0, 0)])?;
    }
    Ok(())
}

fn button(name: &str) -> KeyCode {
    match name {
        "0" | "left" => KeyCode::BTN_LEFT,
        "1" | "middle" => KeyCode::BTN_MIDDLE,
        "2" | "right" => KeyCode::BTN_RIGHT,
        _ => KeyCode::BTN_LEFT,
    }
}

fn button_event(btn: KeyCode, value: i32) -> InputEvent {
    InputEvent::new(EventType::KEY.0, btn.code(), value)
}

fn key_event(code: u16, value: i32) -> InputEvent {
    InputEvent::new(EventType::KEY.0, code, value)
}
