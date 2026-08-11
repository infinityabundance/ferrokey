//! Court target: a Slint window with a text input.
//!
//! Built with Slint's normal winit backend — this is a *regular* application
//! that the OSK must not steal focus from.

use ferrokey_test_common::{Reporter, TargetEvent};
use slint::{ComponentHandle, SharedString};
use std::sync::Arc;

slint::slint! {
    export component App inherits Window {
        title: "ferrokey-test-target-slint";
        width: 420px;
        height: 120px;
        background: #203040;

        callback focus-changed(bool);
        callback text-changed(string);
        callback key-injected(string);

        VerticalLayout {
            padding: 16px;
            Text {
                text: "ferrokey test target (slint)";
                color: white;
                font-size: 16px;
            }
            TextInput {
                placeholder-text: "type here";
                color: white;
                background: #304050;
                border-radius: 4px;
                padding: 8px;
                focus-changed(f) => { root.focus-changed(f); }
                edited(t) => { root.text-changed(t); }
                key-pressed(event) => {
                    root.key-injected(event.text);
                    reject
                }
            }
        }
    }
}

fn main() {
    let reporter = Arc::new(Reporter::bind().expect("bind reporter socket"));
    reporter.spawn_accept_loop();
    reporter.report(TargetEvent::Ready);

    let app = App::new().expect("create slint window");

    let r = reporter.clone();
    app.on_focus_changed(move |focused| r.focus(focused));
    let r = reporter.clone();
    app.on_text_changed(move |text: SharedString| r.text(&text));
    let r = reporter.clone();
    app.on_key_injected(move |text: SharedString| {
        for ch in text.chars() {
            r.ch(ch);
        }
    });

    app.run().expect("run slint event loop");
}
