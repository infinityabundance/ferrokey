//! Debug probe: a layer surface with `keyboard_interactivity = none` that
//! prints every pointer/keyboard event it receives. Used to isolate whether a
//! compositor honours `keyboard_interactivity = none` when the layer surface
//! is clicked (the Ferrokey OSK contract).

use wayland_client::delegate_noop;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{
    Layer, ZwlrLayerShellV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1 as zls;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

const NAMESPACE: &str = "ferrokey-probe";

struct State {
    compositor: Option<wl_compositor::WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    shm: Option<wl_shm::WlShm>,
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<ZwlrLayerSurfaceV1>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    configured: bool,
}

fn log(msg: &str) {
    // stderr is unbuffered: the probe's output must survive a timeout kill.
    eprintln!("[probe] {msg}");
}

impl State {
    fn maybe_create(&mut self, qh: &QueueHandle<State>) {
        if self.layer_surface.is_some() {
            return;
        }
        let (Some(compositor), Some(layer_shell), Some(_shm)) =
            (&self.compositor, &self.layer_shell, &self.shm)
        else {
            return;
        };
        let surface = compositor.create_surface(qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            Layer::Overlay,
            NAMESPACE.to_string(),
            qh,
            (),
        );
        layer_surface.set_anchor(Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer_surface.set_size(920, 342);
        surface.commit();
        self.surface = Some(surface);
        self.layer_surface = Some(layer_surface);
        log("layer surface created (keyboard_interactivity=none, 920x342)");
    }

    /// Map the surface with a tiny opaque shm buffer (zero-filled ARGB).
    fn map(&mut self, qh: &QueueHandle<State>) {
        let (Some(surface), Some(shm)) = (&self.surface, &self.shm) else {
            return;
        };
        use std::os::unix::io::AsFd as _;
        let size = 920 * 342 * 4;
        let path = std::env::temp_dir().join(format!("probe-shm-{}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .expect("create shm file");
        let _ = std::fs::remove_file(&path);
        file.set_len(size as u64).expect("ftruncate");
        let pool = shm.create_pool(file.as_fd(), size, qh, ());
        let buf = pool.create_buffer(0, 920, 342, 920 * 4, wl_shm::Format::Argb8888, qh, ());
        surface.attach(Some(&buf), 0, 0);
        surface.commit();
        log("layer surface mapped");
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match &interface[..] {
                "wl_compositor" => {
                    let c: wl_compositor::WlCompositor = registry.bind(name, 6, qh, ());
                    state.compositor = Some(c);
                    state.maybe_create(qh);
                }
                "wl_shm" => {
                    let s: wl_shm::WlShm = registry.bind(name, 1, qh, ());
                    state.shm = Some(s);
                    state.maybe_create(qh);
                }
                "wl_seat" => {
                    let seat: wl_seat::WlSeat = registry.bind(name, 7, qh, ());
                    state.pointer = Some(seat.get_pointer(qh, ()));
                    state.keyboard = Some(seat.get_keyboard(qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    let l: ZwlrLayerShellV1 = registry.bind(name, 1, qh, ());
                    state.layer_shell = Some(l);
                    state.maybe_create(qh);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        state: &mut Self,
        p: &ZwlrLayerSurfaceV1,
        e: zls::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        if let zls::Event::Configure {
            serial,
            width,
            height,
        } = e
        {
            p.ack_configure(serial);
            log(&format!("configured {width}x{height}"));
            if !state.configured {
                state.configured = true;
                state.map(qh);
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        _s: &mut Self,
        _p: &wl_pointer::WlPointer,
        e: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match e {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                log(&format!("POINTER ENTER at ({surface_x:.0},{surface_y:.0})"));
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                log(&format!(
                    "POINTER MOTION at ({surface_x:.0},{surface_y:.0})"
                ));
            }
            wl_pointer::Event::Button { button, state, .. } => {
                let pressed = matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                log(&format!(
                    "POINTER BUTTON {button:#x} {}",
                    if pressed { "DOWN" } else { "UP" }
                ));
            }
            wl_pointer::Event::Leave { .. } => log("POINTER LEAVE"),
            _ => {}
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        _s: &mut Self,
        _p: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match e {
            wl_keyboard::Event::Enter { .. } => {
                log("KEYBOARD ENTER (layer surface got keyboard focus!)")
            }
            wl_keyboard::Event::Leave { .. } => log("KEYBOARD LEAVE"),
            wl_keyboard::Event::Key { key, state, .. } => {
                let down = matches!(state, WEnum::Value(wl_keyboard::KeyState::Pressed));
                log(&format!("KEY {key} {}", if down { "DOWN" } else { "UP" }));
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore wl_seat::WlSeat);
delegate_noop!(State: ignore ZwlrLayerShellV1);

fn main() {
    let conn = Connection::connect_to_env().expect("connect to wayland");
    let (_, mut queue) = registry_queue_init::<State>(&conn).expect("registry init");
    let _qh = queue.handle();

    let mut state = State {
        compositor: None,
        layer_shell: None,
        shm: None,
        surface: None,
        layer_surface: None,
        keyboard: None,
        pointer: None,
        configured: false,
    };
    loop {
        let _ = queue.blocking_dispatch(&mut state).expect("dispatch");
    }
}
