//! Court target: a Wayland `xdg_toplevel` window reporting focus and keys.
//!
//! `wl_keyboard` enter/leave tell us when the compositor assigns us keyboard
//! focus — the core focus-preservation oracle for the Wayland courts.

use ferrokey_test_common::{Reporter, TargetEvent};
use wayland_client::delegate_noop;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

/// Window size: fills the court output (1280x720) so any click on the
/// compositor surface focuses it — KWin places new toplevels at a
/// compositor-chosen position, and the court clicks a fixed point.
const WIN_W: u32 = 1280;
const WIN_H: u32 = 720;

struct State {
    reporter: Reporter,
    compositor: Option<wl_compositor::WlCompositor>,
    wm_base: Option<(u32, xdg_wm_base::XdgWmBase)>,
    toplevel: Option<(xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel)>,
    surface: Option<wl_surface::WlSurface>,
    buffer: Option<wl_buffer::WlBuffer>,
    configured: bool,
    shm: Option<wl_shm::WlShm>,
}

fn main() {
    let reporter = Reporter::bind().expect("bind reporter socket");
    reporter.spawn_accept_loop();
    reporter.report(TargetEvent::Ready);

    let conn = Connection::connect_to_env().expect("connect to wayland");
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let display = conn.display();
    display.get_registry(&qh, ());

    let mut state = State {
        reporter,
        compositor: None,
        wm_base: None,
        toplevel: None,
        surface: None,
        buffer: None,
        configured: false,
        shm: None,
    };

    loop {
        let _ = queue.blocking_dispatch(&mut state).expect("dispatch");
    }
}

impl State {
    fn maybe_init_window(&mut self, qh: &QueueHandle<State>) {
        if self.toplevel.is_some() {
            return;
        }
        let (Some(compositor), Some((_, wm_base)), Some(shm)) =
            (&self.compositor, &self.wm_base, &self.shm)
        else {
            return;
        };
        let surface = compositor.create_surface(qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, qh, ());
        let toplevel = xdg_surface.get_toplevel(qh, ());
        toplevel.set_title("ferrokey-test-target-wayland".into());
        // A real, full-output window: the surface needs geometry AND an input
        // region before the compositor can ever give it keyboard focus (an
        // empty surface is not clickable).
        toplevel.set_min_size(WIN_W as i32, WIN_H as i32);
        let pool = create_shm_pool(shm, qh);
        let stride = (WIN_W * 4) as i32;
        let buffer = pool.create_buffer(
            0,
            WIN_W as i32,
            WIN_H as i32,
            stride,
            wl_shm::Format::Argb8888,
            qh,
            (),
        );
        // The initial-configure handshake: wlroots sends the first
        // xdg_surface.configure only in response to the FIRST commit of the
        // surface. Committing with the buffer attached before that configure
        // is a protocol error ("xdg_surface has never been configured"; KWin
        // tolerated the early attach, wlroots enforces it), while never
        // committing deadlocks the handshake and the window never maps.
        // The protocol-correct trigger is an EMPTY commit (no attach) here,
        // then attach + commit in the first Configure handler.
        surface.commit();
        self.surface = Some(surface);
        self.buffer = Some(buffer);
        self.toplevel = Some((xdg_surface, toplevel));
    }
}

/// Create a `wl_shm` pool backed by a temporary file. The buffer is left
/// transparent (zeroed); a surface's input region defaults to its buffer
/// bounds, so the window is clickable/focusable even when invisible.
fn create_shm_pool(shm: &wl_shm::WlShm, qh: &QueueHandle<State>) -> wl_shm_pool::WlShmPool {
    use std::os::unix::io::AsFd as _;

    let size = (WIN_W * WIN_H * 4) as usize;
    let path = std::env::temp_dir().join(format!("ferrokey-target-shm-{}", std::process::id()));
    // Open read+write: the compositor mmaps the received fd PROT_READ, which
    // fails (EACCES) on an O_WRONLY file.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .expect("create shm file");
    let _ = std::fs::remove_file(&path); // the fd keeps the file alive
    file.set_len(size as u64).expect("ftruncate shm file");
    // The request moves a duplicate of the fd to the compositor over the wire.
    shm.create_pool(file.as_fd(), size as i32, qh, ())
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name, interface, ..
        } = event
        {
            match &interface[..] {
                "wl_compositor" => {
                    let compositor: wl_compositor::WlCompositor = registry.bind(name, 1, qh, ());
                    state.compositor = Some(compositor);
                    state.maybe_init_window(qh);
                }
                "wl_seat" => {
                    registry.bind::<wl_seat::WlSeat, _, _>(name, 7, qh, ());
                }
                "wl_shm" => {
                    let shm: wl_shm::WlShm = registry.bind(name, 1, qh, ());
                    state.shm = Some(shm);
                    state.maybe_init_window(qh);
                }
                "xdg_wm_base" => {
                    let wm_base: xdg_wm_base::XdgWmBase = registry.bind(name, 1, qh, ());
                    state.wm_base = Some((name, wm_base));
                    state.maybe_init_window(qh);
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for State {
    fn event(
        _s: &mut Self,
        _p: &wl_compositor::WlCompositor,
        _e: wl_compositor::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for State {
    fn event(
        _s: &mut Self,
        _p: &wl_surface::WlSurface,
        _e: wl_surface::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _s: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        e: xdg_wm_base::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = e {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for State {
    fn event(
        state: &mut Self,
        xdg: &xdg_surface::XdgSurface,
        e: xdg_surface::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = e {
            xdg.ack_configure(serial);
            // The first configure is the compositor's go-ahead for mapping:
            // only now may the surface be attached + committed (the earlier
            // empty commit only triggered this configure; attaching the
            // buffer before it would be a protocol error). Later configures
            // are acked but nothing is redrawn (we have no new content).
            if !state.configured {
                if let (Some(surface), Some(buffer)) = (&state.surface, &state.buffer) {
                    surface.attach(Some(buffer), 0, 0);
                    surface.commit();
                }
                state.configured = true;
            }
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        _s: &mut Self,
        _p: &xdg_toplevel::XdgToplevel,
        e: xdg_toplevel::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = e {
            // The court owns the window lifecycle; nothing to do.
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _state: &mut Self,
        seat: &wl_seat::WlSeat,
        e: wl_seat::Event,
        _d: &(),
        _c: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { .. } = e {
            seat.get_keyboard(qh, ());
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _p: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<Self>,
    ) {
        match e {
            wl_keyboard::Event::Enter { .. } => state.reporter.focus(true),
            wl_keyboard::Event::Leave { .. } => state.reporter.focus(false),
            wl_keyboard::Event::Key {
                key, state: kstate, ..
            } => {
                let down = matches!(kstate, WEnum::Value(wl_keyboard::KeyState::Pressed));
                state.reporter.key(key, down);
            }
            _ => {}
        }
    }
}

delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_buffer::WlBuffer);
