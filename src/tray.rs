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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use zbus::zvariant::Type;

/// Commands the tray can send to the UI loop. `Toggle` is the left-click
/// show/hide; `Quit` comes from the right-click context menu's Close item
/// (the dbusmenu below) and exits the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    None,
    Toggle,
    Quit,
}

/// The dbusmenu object path served under the item's `Menu` property (the
/// right-click context menu). Hosts read the path and render the menu via
/// `com.canonical.dbusmenu`.
const MENU_PATH: &str = "/StatusNotifierMenu";

/// One dbusmenu layout item: `(id, properties, children)`.
///
/// This is a REAL struct (not a `Value`) on purpose: the GetLayout reply
/// must be the tuple `(revision: u, layout: (ia{sv}av))` — a bare struct,
/// exactly what KDE's tray importer (`QDBusPendingReply<uint,
/// DBusMenuLayoutItem>`) and the reference `ksni` implementation emit. The
/// previous code returned a single `Value`, which serialized as a bare
/// variant (`v`) with no revision; hosts rejected that reply and never
/// rendered the menu.
#[derive(Debug, Default, Type, Serialize)]
struct MenuLayout {
    id: i32,
    properties: HashMap<String, zbus::zvariant::Value<'static>>,
    children: Vec<zbus::zvariant::Value<'static>>,
}

impl From<MenuLayout> for zbus::zvariant::Value<'static> {
    /// Children are stored as variants (the spec's `av` array): wrapping
    /// each item in a `Value` is what makes the wire format `av` instead of
    /// `a(ia{sv}av)`, which hosts would reject.
    fn from(item: MenuLayout) -> Self {
        use zbus::zvariant::{StructureBuilder, Value};
        Value::from(
            StructureBuilder::new()
                .add_field(item.id)
                .add_field(item.properties)
                .add_field(item.children)
                .build()
                .expect("dbusmenu layout item always has its three fields"),
        )
    }
}
/// The one menu item: Close (id 1; 0 is the root).
const ITEM_CLOSE: i32 = 1;

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

    // Legacy/alternate context-menu hook (some hosts ask the ITEM to show
    // its menu). Modern hosts (KDE Plasma, GNOME) render the menu from the
    // `Menu` property instead, which is the path we implement; this method
    // stays a no-op.
    fn context_menu(&self, x: i32, y: i32) {
        let _ = (x, y);
    }

    /// The right-click context menu (com.canonical.dbusmenu at MENU_PATH).
    #[zbus(property)]
    fn menu(&self) -> zbus::zvariant::ObjectPath<'_> {
        zbus::zvariant::ObjectPath::try_from(MENU_PATH)
            .expect("static menu path is a valid object path")
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
        // Unique per instance: two keyboards can run side by side and each
        // gets its own tray icon (hosts key items on name + id).
        format!("ferrokey-{}", std::process::id())
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

/// The right-click context menu (`com.canonical.dbusmenu`, served at
/// MENU_PATH under the item's `Menu` property). Minimal by design: one
/// Close item. The `commands` slot is shared with the item, so a menu
/// click sets the same command channel the left-click toggle uses.
struct Menu {
    commands: Arc<Mutex<TrayCommand>>,
}

impl Menu {
    /// The Close item's property dict, filtered by the requested names
    /// (the spec: a host may ask for a subset; an empty request = all).
    fn close_properties(
        wanted: &dyn Fn(&str) -> bool,
    ) -> HashMap<String, zbus::zvariant::Value<'static>> {
        use zbus::zvariant::Value;
        let mut props = HashMap::new();
        if wanted("label") {
            props.insert("label".into(), Value::from("Close"));
        }
        if wanted("enabled") {
            props.insert("enabled".into(), Value::from(true));
        }
        if wanted("visible") {
            props.insert("visible".into(), Value::from(true));
        }
        props
    }
}

// The dbusmenu protocol. Method names are zbus-converted to the DBus
// CamelCase (get_layout -> GetLayout, ...). Return shapes follow the
// com.canonical.dbusmenu spec:
//   GetLayout            -> (revision, layout) with layout a variant `v`
//                          wrapping the (id, a{sv}, av) item struct;
//   GetGroupProperties   -> a(ia{sv}av);
//   Event / GetProperty  -> side effects / a variant.
#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> (u32, MenuLayout) {
        let _ = (parent_id, recursion_depth);
        let wanted =
            |key: &str| property_names.is_empty() || property_names.iter().any(|p| p == key);
        // Root (id 0, no properties) with one child: the Close item.
        let child = MenuLayout {
            id: ITEM_CLOSE,
            properties: Self::close_properties(&wanted),
            children: Vec::new(),
        };
        (
            0u32,
            MenuLayout {
                id: 0,
                properties: HashMap::new(),
                children: vec![child.into()],
            },
        )
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(
        i32,
        HashMap<String, zbus::zvariant::Value<'static>>,
        Vec<zbus::zvariant::Value<'static>>,
    )> {
        // Only the Close item exists; any other id is reported as an empty
        // (invisible) item so hosts do not treat it as missing. The property
        // names filter the returned dict the same way GetLayout filters it
        // (the spec's `as propertyNames` argument).
        let wanted =
            |key: &str| property_names.is_empty() || property_names.iter().any(|p| p == key);
        ids.into_iter()
            .map(|id| {
                if id == ITEM_CLOSE {
                    (id, Self::close_properties(&wanted), Vec::new())
                } else {
                    (id, HashMap::new(), Vec::new())
                }
            })
            .collect()
    }

    fn event(&self, id: i32, event_id: String, data: zbus::zvariant::Value<'_>, timestamp: u32) {
        let _ = (data, timestamp);
        if id == ITEM_CLOSE && event_id == "clicked" {
            *self.commands.lock().unwrap() = TrayCommand::Quit;
        }
    }

    fn event_group(
        &self,
        ids: Vec<i32>,
        event_id: String,
        data: zbus::zvariant::Value<'_>,
        timestamp: u32,
    ) -> i32 {
        let _ = (data, timestamp);
        // Report each id as handled the same way `event` handles it.
        let mut handled = 0;
        for id in ids {
            if id == ITEM_CLOSE && event_id == "clicked" {
                *self.commands.lock().unwrap() = TrayCommand::Quit;
                handled += 1;
            }
        }
        handled
    }

    fn get_property(&self, id: i32, property: String) -> zbus::zvariant::Value<'static> {
        use zbus::zvariant::Value;
        if id != ITEM_CLOSE {
            return Value::from("");
        }
        match property.as_str() {
            "label" => Value::from("Close"),
            "enabled" | "visible" => Value::from(true),
            _ => Value::from(""),
        }
    }

    fn about_to_show(&self, id: i32) -> bool {
        let _ = id;
        true
    }

    #[zbus(property)]
    fn version(&self) -> i32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> String {
        "ltr".into()
    }
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
        // The right-click context menu (a host that reads the item's Menu
        // property renders this dbusmenu; failure to register just means no
        // menu, never a broken tray).
        let _ = conn.object_server().at(
            MENU_PATH,
            Menu {
                commands: commands.clone(),
            },
        );
        // Unique per instance: two keyboards can run side by side and each
        // gets its own tray icon (hosts key items on name + id). The pid
        // suffix must NOT start the final element — bus-name elements may
        // not begin with a digit (the tray host would never find the item).
        let name = format!("org.ferrokey.StatusNotifierItem-{}", std::process::id());
        // zbus 5: `request_name` uses the default request flags
        // (AllowReplacement | ReplaceExisting | DoNotQueue), i.e. the same
        // replace-an-existing-owner policy the v4 API exposed explicitly.
        if let Err(e) = conn.request_name(name.as_str()) {
            log::warn!("tray item name not acquired ({name}): {e}");
        }
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
