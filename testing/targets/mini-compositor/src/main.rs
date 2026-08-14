//! ferrokey-test-mini-compositor — the "Wayland session WITHOUT
//! zwlr_layer_shell_v1" fixture for the backend-selection court (§65/§66 of
//! the addendum).
//!
//! A deliberately minimal Wayland compositor: it advertises the core
//! globals (`wl_compositor`, `wl_shm`, `wl_seat`, `wl_output`,
//! `wl_subcompositor`) and NOTHING else — in particular NOT
//! `zwlr_layer_shell_v1` — so Ferrokey's capability detection must observe
//! "Wayland session without layer-shell" and select the XWayland fallback or
//! degraded mode, deterministically.
//!
//! The fixture only needs to serve the registry global list and roundtrips:
//! the selection court never creates surfaces on it (a client that binds a
//! global and requests resources is answered with a logged no-op — the
//! compositor makes no rendering or input promises).
//!
//! Usage: `ferrokey-test-mini-compositor [socket-name]` — the socket is
//! created as `$XDG_RUNTIME_DIR/<name>` (default `ferrokey-mini`). The
//! process runs until killed; the court terminates it after the assertion.

use std::env;
use std::os::unix::io::AsRawFd;
use std::sync::Arc;
use std::time::Duration;

use wayland_server::backend::ClientData;
use wayland_server::protocol::{wl_compositor, wl_output, wl_seat, wl_shm, wl_subcompositor};
use wayland_server::{
    Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, ListeningSocket, New,
};

/// The compositor state: intentionally empty — there is nothing to track.
struct State;

/// Client bookkeeping: none needed for a detection fixture.
struct NoopClientData;

impl ClientData for NoopClientData {}

fn log(msg: &str) {
    // stderr is unbuffered: the fixture's output must survive a kill.
    eprintln!("[mini-compositor] {msg}");
}

// ── global advertisement ────────────────────────────────────────────────────
// Each global is registered with a standard version; none of them is
// zwlr_layer_shell_v1.

fn create_globals(handle: &DisplayHandle) {
    let _ = handle.create_global::<State, wl_compositor::WlCompositor, ()>(4, ());
    let _ = handle.create_global::<State, wl_shm::WlShm, ()>(1, ());
    let _ = handle.create_global::<State, wl_seat::WlSeat, ()>(7, ());
    let _ = handle.create_global::<State, wl_output::WlOutput, ()>(4, ());
    let _ = handle.create_global::<State, wl_subcompositor::WlSubcompositor, ()>(1, ());
    log("globals advertised: compositor, shm, seat, output, subcompositor — NO layer-shell");
}

// ── per-global bind: associate unit data with every new resource ────────────

macro_rules! global_bind {
    ($iface:ty) => {
        impl GlobalDispatch<$iface, ()> for State {
            fn bind(
                _state: &mut Self,
                _handle: &DisplayHandle,
                _client: &Client,
                resource: New<$iface>,
                _global_data: &(),
                data_init: &mut DataInit<'_, Self>,
            ) {
                data_init.init(resource, ());
            }
        }
    };
}

global_bind!(wl_compositor::WlCompositor);
global_bind!(wl_shm::WlShm);
global_bind!(wl_seat::WlSeat);
global_bind!(wl_output::WlOutput);
global_bind!(wl_subcompositor::WlSubcompositor);

// ── per-resource dispatch: no-ops with a log trail (the fixture creates
//    nothing, so request handlers exist only for completeness) ──────────────

macro_rules! noop_dispatch {
    ($iface:ty) => {
        impl Dispatch<$iface, ()> for State {
            fn request(
                _state: &mut Self,
                client: &Client,
                _resource: &$iface,
                request: <$iface as wayland_server::Resource>::Request,
                _data: &(),
                _dhandle: &DisplayHandle,
                _data_init: &mut DataInit<'_, Self>,
            ) {
                log(&format!(
                    "client {client:?}: {} request (ignored)",
                    std::any::type_name::<<$iface as wayland_server::Resource>::Request>()
                ));
                let _ = request;
            }
            fn destroyed(
                _state: &mut Self,
                client: wayland_server::backend::ClientId,
                _resource: &$iface,
                _data: &(),
            ) {
                log(&format!(
                    "client {client:?}: {} destroyed",
                    stringify!($iface)
                ));
            }
        }
    };
}

noop_dispatch!(wl_compositor::WlCompositor);
noop_dispatch!(wl_shm::WlShm);
noop_dispatch!(wl_seat::WlSeat);
noop_dispatch!(wl_output::WlOutput);
noop_dispatch!(wl_subcompositor::WlSubcompositor);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = env::args()
        .nth(1)
        .unwrap_or_else(|| "ferrokey-mini".to_string());
    let mut display: Display<State> = Display::new()?;
    let mut handle = display.handle();
    create_globals(&handle);
    let socket = ListeningSocket::bind(&name).map_err(|e| format!("bind {name:?}: {e}"))?;
    log(&format!(
        "listening on {name} (fd {}); no layer-shell advertised — the detection fixture",
        socket.as_raw_fd()
    ));
    let mut state = State;
    // Block until killed; the court terminates the process after asserting.
    loop {
        while let Some(stream) = socket.accept()? {
            handle.insert_client(stream, Arc::new(NoopClientData))?;
            log("client connected");
        }
        display.dispatch_clients(&mut state)?;
        display.flush_clients()?;
        std::thread::sleep(Duration::from_millis(10));
    }
}
