//! Court target: a Slint window with a text input.
//!
//! Built with Slint's normal winit backend — this is a *regular* application
//! that the OSK must not steal focus from.
//!
//! Slint 1.17 builtin API notes (verified against i-slint-compiler 1.17.1
//! builtins.slint):
//!   * `TextInput` has `has-focus` (out property), `edited` (0-arg callback),
//!     and `key_pressed(event: KeyEvent) -> EventResult`. There is no
//!     `placeholder-text`/`background`/`focus-changed` on the builtin, so the
//!     markup below sticks to the properties that exist.
//!   * Reacting to a property change uses `changed <property> => { … }`.
//!   * `key_pressed` returning `reject` lets `TextInput` process the key
//!     (insert the character), which then fires `edited` — our text oracle.

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
            spacing: 8px;
            Text {
                text: "ferrokey test target (slint)";
                color: white;
                font-size: 16px;
            }
            TextInput {
                color: white;
                font-size: 16px;
                changed has-focus => { root.focus-changed(self.has-focus); }
                edited => { root.text-changed(self.text); }
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
