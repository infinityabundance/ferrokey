//! Wayland surface backend.
//!
//! Connects to the compositor, binds the globals Ferrokey needs
//! (`zwlr_layer_shell_v1`, `wl_compositor`, `wl_shm`, `wl_seat`, `wl_output`),
//! and drives a layer surface with `keyboard_interactivity = none`.
//!
//! Rendering uses `wl_shm` buffers: the Slint software renderer produces
//! `PremultipliedRgbaColor` (memory order R,G,B,A) which matches
//! `wl_shm::Format::Abgr8888` exactly — no pixel conversion needed on the
//! common path.

pub mod layer_shell;

use crate::{PointerButton, Surface, SurfaceBackend, SurfaceError, SurfaceEvent};
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::time::Duration;

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface, wl_touch,
};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1;

/// Maximum surface dimension Ferrokey will ever render (sanity bound).
pub const MAX_DIMENSION: u32 = 8192;

/// Surface-side Wayland state.
pub struct WlState {
    pub events: VecDeque<SurfaceEvent>,
    pub compositor: Option<wl_compositor::WlCompositor>,
    pub shm: Option<wl_shm::WlShm>,
    pub seat: Option<wl_seat::WlSeat>,
    pub pointer: Option<wl_pointer::WlPointer>,
    pub touch: Option<wl_touch::WlTouch>,
    pub output: Option<wl_output::WlOutput>,
    pub layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    pub surface: Option<wl_surface::WlSurface>,
    pub layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    pub configured: bool,
    pub pending_size: (u32, u32),
    pub scale: i32,
    pub visible: bool,
    // Last known touch position (wl_touch::Up carries no coordinates).
    pub last_touch: Option<(f64, f64)>,
    // Last known pointer position (wl_pointer::Button carries no
    // coordinates; the compositor always sends Motion before Button).
    pub last_pointer: Option<(f64, f64)>,
    // wl_shm buffer pool
    pub buffer: Option<wl_buffer::WlBuffer>,
    pub backing: Option<File>,
    pub pool_size: usize,
    pub shm_format: wl_shm::Format,
    /// Formats advertised by the compositor (wl_shm::Format events).
    pub shm_formats: Vec<wl_shm::Format>,
}

impl WlState {
    fn new() -> Self {
        WlState {
            events: VecDeque::new(),
            compositor: None,
            shm: None,
            seat: None,
            pointer: None,
            touch: None,
            output: None,
            layer_shell: None,
            surface: None,
            layer_surface: None,
            configured: false,
            pending_size: (0, 0),
            scale: 1,
            visible: false,
            last_touch: None,
            last_pointer: None,
            buffer: None,
            backing: None,
            pool_size: 0,
            shm_format: wl_shm::Format::Abgr8888,
            shm_formats: Vec::new(),
        }
    }

    /// Choose the buffer format from what the compositor advertises. The
    /// software renderer emits R,G,B,A bytes which match `ABGR8888` memory
    /// order exactly; `ARGB8888`/`XRGB8888` need a per-pixel R↔B swap. Some
    /// compositors (KWin) reject `ABGR8888` entirely, so never hardcode it.
    fn pick_format(&mut self) {
        for preferred in [
            wl_shm::Format::Abgr8888,
            wl_shm::Format::Argb8888,
            wl_shm::Format::Xrgb8888,
        ] {
            if self.shm_formats.contains(&preferred) {
                self.shm_format = preferred;
                return;
            }
        }
        if let Some(first) = self.shm_formats.first() {
            self.shm_format = *first;
        }
    }

    fn bind_input(&mut self, qh: &QueueHandle<WlState>) {
        if let Some(seat) = &self.seat {
            if self.pointer.is_none() {
                self.pointer = Some(seat.get_pointer(qh, ()));
            }
            if self.touch.is_none() {
                self.touch = Some(seat.get_touch(qh, ()));
            }
        }
    }
}

/// The full Wayland surface.
pub struct WaylandSurface {
    conn: Connection,
    queue: EventQueue<WlState>,
    state: WlState,
    width: u32,
    height: u32,
    ready: bool,
}

impl WaylandSurface {
    /// Connect to the Wayland compositor and bind the required globals.
    pub fn connect() -> Result<Self, SurfaceError> {
        let conn = Connection::connect_to_env()
            .map_err(|e| SurfaceError::Connect(format!("WAYLAND_DISPLAY: {e}")))?;
        let (globals, mut queue) = registry_queue_init::<WlState>(&conn)
            .map_err(|e| SurfaceError::Connect(e.to_string()))?;

        let mut state = WlState::new();
        let qh = queue.handle();

        let compositor = globals
            .bind::<wl_compositor::WlCompositor, WlState, ()>(&qh, 1..=6, ())
            .map_err(|e| SurfaceError::Missing(format!("wl_compositor: {e}")))?;
        let shm = globals
            .bind::<wl_shm::WlShm, WlState, ()>(&qh, 1..=1, ())
            .map_err(|e| SurfaceError::Missing(format!("wl_shm: {e}")))?;
        let layer_shell = globals
            .bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, WlState, ()>(&qh, 1..=1, ())
            .map_err(|e| SurfaceError::Missing(format!("zwlr_layer_shell_v1: {e}")))?;
        let seat = globals
            .bind::<wl_seat::WlSeat, WlState, ()>(&qh, 1..=7, ())
            .ok();
        let output = globals
            .bind::<wl_output::WlOutput, WlState, ()>(&qh, 1..=4, ())
            .ok();

        state.compositor = Some(compositor);
        state.shm = Some(shm);
        state.layer_shell = Some(layer_shell);
        state.seat = seat;
        state.output = output;

        // Round-trip so seat capabilities are known.
        queue
            .roundtrip(&mut state)
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        state.pick_format();
        log::info!("wayland shm format: {:?}", state.shm_format);
        state.bind_input(&qh);

        Ok(WaylandSurface {
            conn,
            queue,
            state,
            width: 0,
            height: 0,
            ready: false,
        })
    }

    fn qh(&self) -> QueueHandle<WlState> {
        self.queue.handle()
    }

    /// Ensure the wl_surface and layer surface exist, and set the requested
    /// geometry (anchored bottom, keyboard_interactivity = none).
    fn ensure_created(&mut self, width: u32, height: u32) -> Result<(), SurfaceError> {
        if self.state.surface.is_none() {
            let compositor = self.state.compositor.as_ref().unwrap().clone();
            let surface = compositor.create_surface(&self.qh(), ());
            self.state.surface = Some(surface);
        }
        if self.state.layer_surface.is_none() {
            let layer_shell = self.state.layer_shell.as_ref().unwrap().clone();
            let surface = self.state.surface.as_ref().unwrap().clone();
            let output = self.state.output.clone();
            let layer_surface = layer_shell::create_layer_surface(
                &layer_shell,
                &surface,
                output.as_ref(),
                width,
                height,
                &self.qh(),
            );
            self.state.layer_surface = Some(layer_surface);
        }
        let layer_surface = self.state.layer_surface.as_ref().unwrap();
        layer_surface.set_size(width, height);
        self.state.pending_size = (width, height);
        if let Some(surface) = &self.state.surface {
            surface.commit();
        }
        Ok(())
    }
}

impl Surface for WaylandSurface {
    fn backend(&self) -> SurfaceBackend {
        SurfaceBackend::WaylandLayerShell
    }

    fn set_size(&mut self, width: u32, height: u32) -> Result<(), SurfaceError> {
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION {
            return Err(SurfaceError::Unsupported(format!(
                "invalid size {width}x{height}"
            )));
        }
        self.width = width;
        self.height = height;
        if self.ready {
            self.ensure_created(width, height)?;
        }
        Ok(())
    }

    fn present(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        stride: u32,
    ) -> Result<(), SurfaceError> {
        self.state.bind_input(&self.qh());
        if self.state.layer_surface.is_none() {
            self.ensure_created(width, height)?;
        }
        let bytes = (height as usize)
            .checked_mul(stride as usize)
            .ok_or_else(|| SurfaceError::Unsupported("size overflow".into()))?;
        if self.state.pool_size != bytes {
            let shm = self.state.shm.as_ref().unwrap().clone();
            let fd = create_memfd(bytes)?;
            let pool = shm.create_pool(fd.as_fd(), bytes as i32, &self.qh(), ());
            let buf = pool.create_buffer(
                0,
                width as i32,
                height as i32,
                stride as i32,
                self.state.shm_format,
                &self.qh(),
                (),
            );
            drop(pool); // the buffer references the pool server-side
                        // Own the fd as a writable file: the compositor reads the same
                        // page cache via its own mapping, so writing is equivalent to
                        // memcpy into shared memory.
            let backing = File::from(fd);
            self.state.buffer = Some(buf);
            self.state.backing = Some(backing);
            self.state.pool_size = bytes;
        }
        // Copy pixels into the shared buffer. The software renderer emits
        // R,G,B,A bytes; ABGR8888 matches that memory order exactly, any
        // other format (ARGB8888/XRGB8888) needs a per-pixel R↔B swap.
        if let Some(backing) = &mut self.state.backing {
            backing
                .seek(SeekFrom::Start(0))
                .map_err(|e| SurfaceError::Io(e.to_string()))?;
            // The renderer emits R,G,B,A bytes: ABGR8888 matches directly,
            // ARGB8888/XRGB8888 need a per-pixel R↔B swap.
            if self.state.shm_format == wl_shm::Format::Abgr8888 {
                backing
                    .write_all(&buffer[..bytes])
                    .map_err(|e| SurfaceError::Io(e.to_string()))?;
            } else {
                let mut swapped = buffer.to_vec();
                for px in swapped.chunks_exact_mut(4) {
                    px.swap(0, 2);
                }
                backing
                    .write_all(&swapped)
                    .map_err(|e| SurfaceError::Io(e.to_string()))?;
            }
        }
        let surface = self.state.surface.as_ref().unwrap().clone();
        let buf = self.state.buffer.as_ref().unwrap().clone();
        surface.attach(Some(&buf), 0, 0);
        surface.damage_buffer(0, 0, width as i32, height as i32);
        surface.commit();
        log::debug!("layer surface presented: {width}x{height}");
        self.conn
            .flush()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) -> Result<(), SurfaceError> {
        self.state.visible = visible;
        if visible {
            if self.width == 0 || self.height == 0 {
                return Err(SurfaceError::Unsupported(
                    "set_size must be called before set_visible".into(),
                ));
            }
            self.ensure_created(self.width, self.height)?;
            self.ready = true;
        }
        Ok(())
    }

    fn poll_events(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<Vec<SurfaceEvent>, SurfaceError> {
        let deadline = timeout.map(|t| std::time::Instant::now() + t);
        loop {
            self.state.bind_input(&self.qh());
            // The Wayland read path does NOT flush outgoing requests, so
            // without an explicit flush here the compositor never sees our
            // layer-surface creation/commit requests: the surface is never
            // configured or mapped and the OSK silently never appears.
            self.conn
                .flush()
                .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
            self.queue
                .dispatch_pending(&mut self.state)
                .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
            let events: Vec<SurfaceEvent> = self.state.events.drain(..).collect();
            if !events.is_empty() {
                self.ready = true;
                return Ok(events);
            }
            let remaining = match deadline {
                Some(d) => d.saturating_duration_since(std::time::Instant::now()),
                None => Duration::from_millis(500),
            };
            if let Some(guard) = self.queue.prepare_read() {
                let backend = self.conn.backend();
                let fd = backend.poll_fd();
                let mut fds = [nix::poll::PollFd::new(fd, nix::poll::PollFlags::POLLIN)];
                let timeout_ms = remaining.as_millis().min(u128::from(u16::MAX)) as u16;
                let n = nix::poll::poll(&mut fds, nix::poll::PollTimeout::from(timeout_ms))
                    .map_err(|e| SurfaceError::Io(e.to_string()))?;
                if n == 0 {
                    return Ok(Vec::new());
                }
                guard
                    .read()
                    .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
            } else {
                self.conn
                    .flush()
                    .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
                if remaining == Duration::ZERO {
                    return Ok(Vec::new());
                }
            }
        }
    }

    fn scale_factor(&self) -> f32 {
        self.state.scale.max(1) as f32
    }

    fn is_ready(&self) -> bool {
        self.ready && self.state.configured
    }
}

// ── Dispatch impls ────────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for WlState {
    fn event(
        _state: &mut WlState,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<WlState>,
    ) {
        // Globals are bound eagerly at connect time.
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for WlState {
    fn event(
        _s: &mut WlState,
        _p: &wl_compositor::WlCompositor,
        _e: wl_compositor::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<WlState>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for WlState {
    fn event(
        _s: &mut WlState,
        _p: &wl_surface::WlSurface,
        _e: wl_surface::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<WlState>,
    ) {
    }
}

impl Dispatch<wl_shm::WlShm, ()> for WlState {
    fn event(
        s: &mut WlState,
        _p: &wl_shm::WlShm,
        e: wl_shm::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<WlState>,
    ) {
        if let wl_shm::Event::Format {
            format: WEnum::Value(f),
        } = e
        {
            s.shm_formats.push(f);
        }
    }
}

delegate_noop!(WlState: ignore wl_shm_pool::WlShmPool);
delegate_noop!(WlState: ignore wl_buffer::WlBuffer);

impl Dispatch<wl_seat::WlSeat, ()> for WlState {
    fn event(
        s: &mut WlState,
        seat: &wl_seat::WlSeat,
        e: wl_seat::Event,
        _d: &(),
        _c: &Connection,
        qh: &QueueHandle<WlState>,
    ) {
        if let wl_seat::Event::Capabilities { .. } = e {
            if s.pointer.is_none() {
                s.pointer = Some(seat.get_pointer(qh, ()));
            }
            if s.touch.is_none() {
                s.touch = Some(seat.get_touch(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for WlState {
    fn event(
        s: &mut WlState,
        _p: &wl_pointer::WlPointer,
        e: wl_pointer::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<WlState>,
    ) {
        match e {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                // KWin's x11 backend delivers the position on Enter and may
                // not send Motion before Button, so record Enter coordinates
                // too (wl_pointer::Button carries none).
                s.last_pointer = Some((surface_x, surface_y));
                log::debug!("layer pointer enter at ({surface_x:.0},{surface_y:.0})");
                s.events.push_back(SurfaceEvent::PointerMoved {
                    x: surface_x,
                    y: surface_y,
                });
            }
            wl_pointer::Event::Leave { .. } => {
                s.last_pointer = None;
                s.events.push_back(SurfaceEvent::PointerLeft);
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                s.last_pointer = Some((surface_x, surface_y));
                log::debug!("layer pointer motion at ({surface_x:.0},{surface_y:.0})");
                s.events.push_back(SurfaceEvent::PointerMoved {
                    x: surface_x,
                    y: surface_y,
                });
            }
            wl_pointer::Event::Button { button, state, .. } => {
                let button = match button {
                    0x110 => PointerButton::Left,
                    0x111 => PointerButton::Middle,
                    0x112 => PointerButton::Right,
                    _ => return,
                };
                // wl_pointer::Button carries no coordinates; the compositor
                // always precedes it with a Motion, so use the last position
                // (falling back to 0,0 only if no motion was ever seen).
                let (x, y) = s.last_pointer.unwrap_or((0.0, 0.0));
                log::debug!("layer pointer button {button:?} at ({x:.0},{y:.0})");
                match state {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => {
                        s.events
                            .push_back(SurfaceEvent::PointerPressed { x, y, button });
                    }
                    WEnum::Value(wl_pointer::ButtonState::Released) => {
                        s.events
                            .push_back(SurfaceEvent::PointerReleased { x, y, button });
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_touch::WlTouch, ()> for WlState {
    fn event(
        s: &mut WlState,
        _p: &wl_touch::WlTouch,
        e: wl_touch::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<WlState>,
    ) {
        match e {
            wl_touch::Event::Down { x, y, .. } => {
                log::debug!("layer touch down at ({x:.0},{y:.0})");
                s.last_touch = Some((x, y));
                s.events.push_back(SurfaceEvent::TouchPressed { x, y });
            }
            wl_touch::Event::Up { .. } => {
                log::debug!("layer touch up");
                // wl_touch::Up carries no coordinates; replay the last known
                // position so hit-testing state stays consistent.
                let (x, y) = s.last_touch.unwrap_or((0.0, 0.0));
                s.last_touch = None;
                s.events.push_back(SurfaceEvent::TouchReleased { x, y });
            }
            wl_touch::Event::Motion { x, y, .. } => {
                log::debug!("layer touch motion at ({x:.0},{y:.0})");
                s.last_touch = Some((x, y));
                s.events.push_back(SurfaceEvent::TouchMoved { x, y });
            }
            wl_touch::Event::Cancel => {
                log::debug!("layer touch cancel");
                s.last_touch = None;
                s.events.push_back(SurfaceEvent::TouchCancelled);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for WlState {
    fn event(
        s: &mut WlState,
        _p: &wl_output::WlOutput,
        e: wl_output::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<WlState>,
    ) {
        if let wl_output::Event::Scale { factor } = e {
            s.scale = factor;
        }
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for WlState {
    fn event(
        _s: &mut WlState,
        _p: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _e: zwlr_layer_shell_v1::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<WlState>,
    ) {
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for WlState {
    fn event(
        s: &mut WlState,
        p: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        e: zwlr_layer_surface_v1::Event,
        _d: &(),
        _c: &Connection,
        _q: &QueueHandle<WlState>,
    ) {
        match e {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                p.ack_configure(serial);
                let width = if width == 0 { s.pending_size.0 } else { width };
                let height = if height == 0 {
                    s.pending_size.1
                } else {
                    height
                };
                log::info!("layer surface configured: {width}x{height}");
                s.configured = true;
                s.events.push_back(SurfaceEvent::Resized { width, height });
            }
            zwlr_layer_surface_v1::Event::Closed => {
                s.events.push_back(SurfaceEvent::CloseRequested);
            }
            _ => {}
        }
    }
}

// ── Shared memory helpers ─────────────────────────────────────────────────

/// Create a memfd of `size` bytes suitable for a wl_shm pool.
fn create_memfd(size: usize) -> Result<OwnedFd, SurfaceError> {
    let name =
        std::ffi::CString::new("ferrokey-osk").map_err(|e| SurfaceError::Io(e.to_string()))?;
    let fd = nix::sys::memfd::memfd_create(name.as_c_str(), nix::sys::memfd::MFdFlags::MFD_CLOEXEC)
        .map_err(|e| SurfaceError::Io(e.to_string()))?;
    nix::unistd::ftruncate(&fd, size as i64).map_err(|e| SurfaceError::Io(e.to_string()))?;
    Ok(fd)
}

/// Probe whether the compositor advertises zwlr_layer_shell_v1.
pub fn probe_globals() -> Result<bool, SurfaceError> {
    let conn = Connection::connect_to_env()
        .map_err(|e| SurfaceError::Connect(format!("WAYLAND_DISPLAY: {e}")))?;
    let (globals, mut queue) = registry_queue_init::<ProbeState>(&conn)
        .map_err(|e| SurfaceError::Connect(e.to_string()))?;
    let mut state = ProbeState;
    queue
        .roundtrip(&mut state)
        .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
    let name = <zwlr_layer_shell_v1::ZwlrLayerShellV1 as wayland_client::Proxy>::interface().name;
    Ok(globals
        .contents()
        .with_list(|list| list.iter().any(|g| g.interface == name)))
}

struct ProbeState;

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for ProbeState {
    fn event(
        _state: &mut ProbeState,
        _proxy: &wl_registry::WlRegistry,
        _event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<ProbeState>,
    ) {
    }
}
