//! Court target: an SDL2 window that reports genuine key state transitions.
//!
//! Games, emulators and SDL applications read raw key-down/key-up events, not
//! text — so this target logs the SDL scancode of every key transition to the
//! reporter socket. The SDL court asserts the ORDER of modifier chords
//! (Ctrl down before C down), which text-only oracles cannot prove.
//!
//! Scancodes are SDL2's own numbering (USB-HID-like): A=4, C=6, F1=58,
//! F5=62, Left=80, Up=82, LeftCtrl=224, RightAlt=230.

use ferrokey_test_common::{Reporter, TargetEvent};
use sdl2::event::Event;
use sdl2::keyboard::Scancode;

fn main() {
    let reporter = Reporter::bind().expect("bind reporter socket");
    reporter.spawn_accept_loop();
    reporter.report(TargetEvent::Ready);

    let sdl = sdl2::init().expect("SDL init");
    let video = sdl.video().expect("SDL video subsystem");
    let window = video
        .window("ferrokey-test-target-sdl", 420, 120)
        .position_centered()
        .build()
        .expect("SDL window");
    // The window never swaps buffers; the event pump still delivers input.
    let _ = window;

    let mut pump = sdl.event_pump().expect("SDL event pump");
    'main: loop {
        for event in pump.poll_iter() {
            match event {
                Event::Quit { .. } => break 'main,
                Event::Window {
                    win_event: sdl2::event::WindowEvent::FocusGained,
                    ..
                } => reporter.focus(true),
                Event::Window {
                    win_event: sdl2::event::WindowEvent::FocusLost,
                    ..
                } => reporter.focus(false),
                Event::KeyDown {
                    scancode: Some(scancode),
                    ..
                } => reporter.key(scancode as u32, true),
                Event::KeyUp {
                    scancode: Some(scancode),
                    ..
                } => reporter.key(scancode as u32, false),
                _ => {}
            }
        }
    }
}

/// Kept referenced: documents the scancode vocabulary the court asserts on.
#[allow(dead_code)]
fn scancode_reference() {
    let _ = (
        Scancode::A as u32,
        Scancode::C as u32,
        Scancode::F5 as u32,
        Scancode::Left as u32,
        Scancode::Up as u32,
        Scancode::LCtrl as u32,
        Scancode::RAlt as u32,
    );
}
