//! The system tray (StatusNotifierItem) presence for the OSK.
//!
//! The OSK is a focus-free overlay (layer-shell / WM_HINTS.input=False), so
//! it deliberately has no taskbar entry — the tray is the user's handle on
//! it: show/hide and quit without hunting for the window. Implemented with
//! the freedesktop StatusNotifierItem protocol on the session bus.
//!
//! The courts sanitize `DBUS_SESSION_BUS_ADDRESS` away, so the tray is a
//! clean no-op there (no tray, no DBus, no extra threads). When a session
//! bus exists, zbus's blocking server runs on its own thread and commands
//! are handed to the UI loop through a mutex — the UI stays single-threaded.

use std::sync::{Arc, Mutex};

/// Commands the tray can send to the UI loop. v1 is show/hide only: there is
/// no Dbusmenu yet, so nothing can construct `Quit`; the toggle covers the
/// convenience a tray exists for. (A future menu would add a Quit command.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    None,
    Toggle,
}

/// The exported `org.kde.StatusNotifierItem` object.
struct NotifierItem {
    commands: Arc<Mutex<TrayCommand>>,
}

// The zbus interface macro generates dispatch code that references the
// method parameter identifiers, so parameters cannot be `_`-prefixed (clippy
// would see them as used underscore bindings); they are named plainly and
// discarded explicitly in the bodies, which never touch them.
#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl NotifierItem {
    /// Left click on the tray icon: show/hide the OSK.
    fn activate(&self, x: i32, y: i32) {
        let _ = (x, y);
        *self.commands.lock().unwrap() = TrayCommand::Toggle;
    }

    /// Middle/scroll click: same toggle.
    fn secondary_activate(&self, x: i32, y: i32) {
        let _ = (x, y);
        *self.commands.lock().unwrap() = TrayCommand::Toggle;
    }

    // A full Dbusmenu is overkill for v1; a menu request is ignored.
    fn context_menu(&self, x: i32, y: i32) {
        let _ = (x, y);
    }

    fn scroll(&self, delta: i32, orientation: String) {
        let _ = (delta, orientation);
    }

    #[zbus(property)]
    fn category(&self) -> String {
        "ApplicationStatus".into()
    }

    #[zbus(property)]
    fn id(&self) -> String {
        "ferrokey".into()
    }

    #[zbus(property)]
    fn title(&self) -> String {
        "Ferrokey on-screen keyboard".into()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        "Active".into()
    }

    /// The freedesktop standard keyboard icon (shipped by KDE and GNOME).
    #[zbus(property)]
    fn icon_name(&self) -> String {
        "input-keyboard".into()
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }
}

/// The tray handle; `start()` returns `None` when no session bus exists.
pub struct Tray {
    /// Kept alive: dropping the connection unregisters the item.
    _conn: zbus::blocking::Connection,
    commands: Arc<Mutex<TrayCommand>>,
}

impl Tray {
    /// Register the StatusNotifierItem on the session bus. Returns `None`
    /// (and logs) when no session bus is available — the UI runs fine
    /// without a tray (terminal-only mode, courts, minimal environments).
    pub fn start() -> Option<Tray> {
        if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
            log::info!("no session dbus: system tray disabled");
            return None;
        }
        let conn = match zbus::blocking::Connection::session() {
            Ok(c) => c,
            Err(e) => {
                log::info!("system tray disabled: session bus unavailable: {e}");
                return None;
            }
        };
        let commands = Arc::new(Mutex::new(TrayCommand::None));
        if conn
            .object_server()
            .at(
                "/StatusNotifierItem",
                NotifierItem {
                    commands: commands.clone(),
                },
            )
            .is_err()
        {
            return None;
        }
        let name = "org.ferrokey.StatusNotifierItem";
        // zbus 5: `request_name` uses the default request flags
        // (AllowReplacement | ReplaceExisting | DoNotQueue), i.e. the same
        // replace-an-existing-owner policy the v4 API exposed explicitly.
        let _ = conn.request_name(name);
        // Tell the status-notifier watcher (the tray host) we exist. Failure
        // is fine: the item simply shows up when a watcher is present.
        let _ = conn.call_method(
            Some("org.kde.StatusNotifierWatcher"),
            "/StatusNotifierWatcher",
            Some("org.kde.StatusNotifierWatcher"),
            "RegisterStatusNotifierItem",
            &name,
        );
        log::info!("system tray registered ({name})");
        Some(Tray {
            _conn: conn,
            commands,
        })
    }

    /// Take the latest command, resetting to `None`.
    pub fn command(&self) -> TrayCommand {
        let mut guard = self.commands.lock().unwrap();
        std::mem::replace(&mut *guard, TrayCommand::None)
    }
}
