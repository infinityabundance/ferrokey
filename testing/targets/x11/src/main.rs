//! Court target: a plain X11 window that reports focus and key events.
//!
//! This is the *target application* of the focus-preservation court: it
//! should own keyboard focus before and after every Ferrokey interaction.

use ferrokey_test_common::{Reporter, TargetEvent};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, EventMask, WindowClass};
use x11rb::protocol::Event;

fn main() {
    let reporter = Reporter::bind().expect("bind reporter socket");
    reporter.spawn_accept_loop();
    reporter.report(TargetEvent::Ready);

    let (conn, screen_num) = x11rb::connect(None).expect("connect to X11");
    let screen = &conn.setup().roots[screen_num];
    let win = conn.generate_id().unwrap();
    conn.create_window(
        screen.root_depth,
        win,
        screen.root,
        100,
        100,
        400,
        120,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new()
            .background_pixel(0x00_30_40)
            .event_mask(
                EventMask::FOCUS_CHANGE
                    | EventMask::KEY_PRESS
                    | EventMask::KEY_RELEASE
                    | EventMask::EXPOSURE
                    | EventMask::STRUCTURE_NOTIFY,
            ),
    )
    .unwrap();
    conn.map_window(win).unwrap();
    conn.flush().unwrap();

    loop {
        let event = conn.wait_for_event().expect("x11 event");
        match event {
            Event::FocusIn(_) => reporter.focus(true),
            Event::FocusOut(_) => reporter.focus(false),
            Event::KeyPress(e) => {
                reporter.key(u32::from(e.detail), true);
            }
            Event::KeyRelease(e) => {
                reporter.key(u32::from(e.detail), false);
            }
            _ => {}
        }
    }
}
