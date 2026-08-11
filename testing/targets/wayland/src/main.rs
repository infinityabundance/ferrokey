//! Court target: a Wayland `xdg_toplevel` window reporting focus and keys.
//!
//! `wl_keyboard` enter/leave tell us when the compositor assigns us keyboard
//! focus — the core focus-preservation oracle for the Wayland courts.

use ferrokey_test_common::{Reporter, TargetEvent};
use wayland_client::delegate_noop;
use wayland_client::protocol::{
    wl_compositor, wl_keyboard, wl_registry, wl_seat, wl_shm, wl_surface,
};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

struct State {
    reporter: Reporter,
    compositor: Option<wl_compositor::WlCompositor>,
    wm_base: Option<(u32, xdg_wm_base::XdgWmBase)>,
    toplevel: Option<(xdg_surface::XdgSurface, xdg_toplevel::XdgToplevel)>,
    configured: bool,
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
        configured: false,
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
        let (Some(compositor), Some((_, wm_base))) = (&self.compositor, &self.wm_base) else {
            return;
        };
        let surface = compositor.create_surface(qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, qh, ());
        let toplevel = xdg_surface.get_toplevel(qh, ());
        toplevel.set_title("ferrokey-test-target-wayland".into());
        surface.commit();
        self.toplevel = Some((xdg_surface, toplevel));
    }
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

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        _state: &mut Self,
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
        if let xdg_surface::Event::Configure { serial, .. } = e {
            xdg.ack_configure(serial);
            state.configured = true;
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for State {
    fn event(
        _s: &mut Self,
        _p: &xdg_toplevel::XdgToplevel,
        _e: xdg_toplevel::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore wl_shm::WlShm);
