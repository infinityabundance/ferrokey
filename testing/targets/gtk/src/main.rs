//! Court target: a GTK3 window with a text entry.
//!
//! Reports focus (via the entry's focus events), text (on `changed`) and raw
//! key events (key-press/release) to the reporter socket.

use ferrokey_test_common::{Reporter, TargetEvent};
use gtk::prelude::*;

fn main() {
    let reporter = std::sync::Arc::new(Reporter::bind().expect("bind reporter socket"));
    reporter.spawn_accept_loop();
    reporter.report(TargetEvent::Ready);

    if gtk::init().is_err() {
        eprintln!("gtk init failed");
        std::process::exit(1);
    }

    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("ferrokey-test-target-gtk");
    window.set_default_size(420, 120);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 8);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let label = gtk::Label::new(Some("ferrokey test target (gtk)"));
    let entry = gtk::Entry::new();

    let r = reporter.clone();
    entry.connect_focus_in_event(move |_, _| {
        r.focus(true);
        glib::Propagation::Proceed
    });
    let r = reporter.clone();
    entry.connect_focus_out_event(move |_, _| {
        r.focus(false);
        glib::Propagation::Proceed
    });
    let r = reporter.clone();
    entry.connect_changed(move |e| {
        r.text(&e.text());
    });
    // gtk-rs 0.18 (GTK3): `EventKey::keyval()` returns `gdk::keys::Key` which
    // derefs to the raw keyval `u32`; the GTK4-era `append()` does not exist on
    // GTK3's `gtk::Box`, so use the GTK3 `Container::add`/`pack_start` API.
    let r = reporter.clone();
    entry.connect_key_press_event(move |_, ev| {
        r.key(*ev.keyval(), true);
        glib::Propagation::Proceed
    });
    let r = reporter.clone();
    entry.connect_key_release_event(move |_, ev| {
        r.key(*ev.keyval(), false);
        glib::Propagation::Proceed
    });

    vbox.pack_start(&label, false, false, 0);
    vbox.pack_start(&entry, false, false, 0);
    window.add(&vbox);

    window.connect_destroy(|_| gtk::main_quit());
    window.show_all();

    gtk::main();
}
