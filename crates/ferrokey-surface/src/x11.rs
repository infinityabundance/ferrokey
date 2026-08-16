//! X11 surface backend (native Xorg and XWayland).
//!
//! X11 has an explicit concept for a no-focus window: the ICCCM **No Input**
//! focus model —
//!
//! ```text
//! WM_HINTS.input = False
//! WM_TAKE_FOCUS absent
//! ```
//!
//! `input = False` requests that the window manager not assign keyboard
//! focus to this top-level window. On top of that Ferrokey sets:
//!
//! ```text
//! _NET_WM_WINDOW_TYPE = _NET_WM_WINDOW_TYPE_DOCK
//! _NET_WM_STATE       = _NET_WM_STATE_ABOVE | _NET_WM_STATE_SKIP_TASKBAR
//!                      | _NET_WM_STATE_SKIP_PAGER
//! ```
//!
//! (and optionally `override_redirect`, controlled by the caller).
//!
//! Rendering uses `XPutImage` with ZPixmap data in the exact wire layout the
//! server expects: little-endian B,G,R for depth 24 (3 bytes/pixel, rows
//! padded to 4 bytes) or B,G,R,X for depth 32. Slint's
//! `PremultipliedRgbaColor` is memory-ordered R,G,B,A, so one R/B swap pass
//! is needed (and the alpha byte is dropped at depth 24).

use crate::touch::TouchTracker;
use crate::{PointerButton, Surface, SurfaceBackend, SurfaceError, SurfaceEvent};
use std::collections::VecDeque;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::properties::WmHints;
use x11rb::protocol::xinput;
use x11rb::protocol::xinput::ConnectionExt as XInputConnectionExt;
use x11rb::protocol::xproto::{
    AtomEnum, ConnectionExt as XProtoConnectionExt, CreateWindowAux, EventMask, ImageFormat,
    PropMode, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;

/// X11 window dimensions.
#[derive(Debug, Clone, Copy)]
pub struct WindowGeometry {
    pub width: u16,
    pub height: u16,
}

/// Options for the X11 window.
#[derive(Debug, Clone)]
pub struct X11Options {
    /// Display string (e.g. `":0"`); `None` uses `$DISPLAY`.
    pub display: Option<String>,
    /// Window title.
    pub title: String,
    /// Bypass the window manager entirely (`override_redirect`).
    pub override_redirect: bool,
}

impl Default for X11Options {
    fn default() -> Self {
        X11Options {
            display: None,
            title: "Ferrokey Virtual Keyboard".into(),
            override_redirect: false,
        }
    }
}

/// A no-focus, above-others X11 window.
pub struct X11Surface {
    conn: RustConnection,
    #[allow(dead_code)]
    screen_num: usize,
    screen_size: (u32, u32),
    window: u32,
    gc: u32,
    width: u32,
    height: u32,
    depth: u8,
    pending_events: VecDeque<SurfaceEvent>,
    visible: bool,
    ready: bool,
    /// Active touch tracking (single-pointer fallback: only the first touch
    /// is forwarded, which matches Slint's single-pointer model).
    touches: TouchTracker,
}

impl X11Surface {
    /// Connect and create the window (unmapped until `set_visible`).
    pub fn create(options: X11Options) -> Result<Self, SurfaceError> {
        let (conn, screen_num) = x11rb::connect(options.display.as_deref())
            .map_err(|e| SurfaceError::Connect(e.to_string()))?;
        let screen = &conn.setup().roots[screen_num];
        let screen_size = (
            u32::from(screen.width_in_pixels),
            u32::from(screen.height_in_pixels),
        );

        let window = conn
            .generate_id()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        let event_mask = EventMask::BUTTON_PRESS
            | EventMask::BUTTON_RELEASE
            | EventMask::POINTER_MOTION
            | EventMask::EXPOSURE
            | EventMask::STRUCTURE_NOTIFY;
        let aux = CreateWindowAux::new()
            .background_pixel(screen.black_pixel)
            .event_mask(event_mask)
            .override_redirect(u32::from(options.override_redirect));
        conn.create_window(
            screen.root_depth,
            window,
            screen.root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &aux,
        )
        .map_err(|e| SurfaceError::Protocol(e.to_string()))?
        .check()
        .map_err(|e| SurfaceError::Protocol(e.to_string()))?;

        let gc = conn
            .generate_id()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        conn.create_gc(gc, window, &x11rb::protocol::xproto::CreateGCAux::default())
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?
            .check()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;

        conn.change_property8(
            PropMode::REPLACE,
            window,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            options.title.as_bytes(),
        )
        .map_err(|e| SurfaceError::Protocol(e.to_string()))?;

        // ICCCM No-Input focus model: never accept keyboard focus.
        WmHints {
            input: Some(false),
            ..Default::default()
        }
        .set(&conn, window)
        .map_err(|e| SurfaceError::Protocol(e.to_string()))?
        .check()
        .map_err(|e| SurfaceError::Protocol(e.to_string()))?;

        Self::set_ewmh(&conn, window)?;

        // WM_DELETE_WINDOW so the WM can request closure cleanly.
        let wm_protocols = conn
            .intern_atom(false, b"WM_PROTOCOLS")
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?
            .reply()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?
            .atom;
        let wm_delete = conn
            .intern_atom(false, b"WM_DELETE_WINDOW")
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?
            .reply()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?
            .atom;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            wm_protocols,
            AtomEnum::ATOM,
            &[wm_delete],
        )
        .map_err(|e| SurfaceError::Protocol(e.to_string()))?;

        conn.flush()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;

        let depth = screen.root_depth;
        let mut surface = X11Surface {
            conn,
            screen_num,
            screen_size,
            window,
            gc,
            width: 0,
            height: 0,
            depth,
            pending_events: VecDeque::new(),
            visible: false,
            ready: false,
            touches: TouchTracker::new(),
        };
        surface.init_xi2_touch()?;
        Ok(surface)
    }

    /// Select XInput2 touch events on the window's master devices.
    ///
    /// Touchscreens (INPUT_PROP_DIRECT devices) deliver touch only through
    /// XI2; there is no core-event equivalent. The mouse and pen keep flowing
    /// through the core pointer masks, so no events are duplicated. Failure is
    /// non-fatal: the OSK simply falls back to pointer-only input (a
    /// touchscreen without XI2 is unusable anyway, but the surface still
    /// works for the mouse).
    fn init_xi2_touch(&mut self) -> Result<(), SurfaceError> {
        // XIAllMasterDevices: touch is delivered on the master device.
        const XI_ALL_MASTER_DEVICES: u16 = 1;
        let version = self
            .conn
            .xinput_xi_query_version(2, 0)
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?
            .reply()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        if version.major_version < 2 {
            log::info!(
                "X11 backend: XInput2 unavailable (major {}); touch disabled",
                version.major_version
            );
            return Ok(());
        }
        let masks = [xinput::EventMask {
            deviceid: XI_ALL_MASTER_DEVICES,
            mask: vec![
                xinput::XIEventMask::TOUCH_BEGIN,
                xinput::XIEventMask::TOUCH_UPDATE,
                xinput::XIEventMask::TOUCH_END,
            ],
        }];
        match self
            .conn
            .xinput_xi_select_events(self.window, &masks)
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?
            .check()
        {
            Ok(()) => {
                log::info!("X11 backend: XInput2 touch selection active");
            }
            Err(e) => {
                log::warn!("X11 backend: XInput2 touch selection failed: {e}");
            }
        }
        Ok(())
    }

    fn touch_down(&mut self, id: u32, x: f64, y: f64) {
        if let Some(event) = self.touches.down(id, x, y) {
            self.pending_events.push_back(event);
        }
    }

    fn touch_move(&mut self, id: u32, x: f64, y: f64) {
        if let Some(event) = self.touches.move_to(id, x, y) {
            self.pending_events.push_back(event);
        }
    }

    fn touch_up(&mut self, id: u32, x: f64, y: f64) {
        for event in self.touches.up(id, x, y) {
            self.pending_events.push_back(event);
        }
    }

    fn set_ewmh(conn: &RustConnection, window: u32) -> Result<(), SurfaceError> {
        let intern = |name: &[u8]| -> Result<u32, SurfaceError> {
            Ok(conn
                .intern_atom(false, name)
                .map_err(|e| SurfaceError::Protocol(e.to_string()))?
                .reply()
                .map_err(|e| SurfaceError::Protocol(e.to_string()))?
                .atom)
        };
        let type_dock = intern(b"_NET_WM_WINDOW_TYPE_DOCK")?;
        let wm_type = intern(b"_NET_WM_WINDOW_TYPE")?;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            wm_type,
            AtomEnum::ATOM,
            &[type_dock],
        )
        .map_err(|e| SurfaceError::Protocol(e.to_string()))?;

        let above = intern(b"_NET_WM_STATE_ABOVE")?;
        let skip_taskbar = intern(b"_NET_WM_STATE_SKIP_TASKBAR")?;
        let skip_pager = intern(b"_NET_WM_STATE_SKIP_PAGER")?;
        let wm_state = intern(b"_NET_WM_STATE")?;
        conn.change_property32(
            PropMode::REPLACE,
            window,
            wm_state,
            AtomEnum::ATOM,
            &[above, skip_taskbar, skip_pager],
        )
        .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        Ok(())
    }

    fn drain_x11(&mut self) -> Result<(), SurfaceError> {
        loop {
            let event = self
                .conn
                .poll_for_event()
                .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
            match event {
                None => break,
                Some(event) => self.handle_event(event),
            }
        }
        Ok(())
    }

    fn handle_event(&mut self, event: Event) {
        match event {
            Event::ButtonPress(e) => {
                let button = match e.detail {
                    1 => PointerButton::Left,
                    2 => PointerButton::Middle,
                    3 => PointerButton::Right,
                    _ => return,
                };
                self.pending_events.push_back(SurfaceEvent::PointerPressed {
                    x: f64::from(e.event_x),
                    y: f64::from(e.event_y),
                    button,
                });
            }
            Event::ButtonRelease(e) => {
                let button = match e.detail {
                    1 => PointerButton::Left,
                    2 => PointerButton::Middle,
                    3 => PointerButton::Right,
                    _ => return,
                };
                self.pending_events
                    .push_back(SurfaceEvent::PointerReleased {
                        x: f64::from(e.event_x),
                        y: f64::from(e.event_y),
                        button,
                    });
            }
            Event::MotionNotify(e) => {
                self.pending_events.push_back(SurfaceEvent::PointerMoved {
                    x: f64::from(e.event_x),
                    y: f64::from(e.event_y),
                });
            }
            // XInput2 touch (touchscreens; only reachable through XI2).
            Event::XinputTouchBegin(e) => {
                self.touch_down(e.detail, fp1616(e.event_x), fp1616(e.event_y));
            }
            Event::XinputTouchUpdate(e) => {
                self.touch_move(e.detail, fp1616(e.event_x), fp1616(e.event_y));
            }
            Event::XinputTouchEnd(e) => {
                self.touch_up(e.detail, fp1616(e.event_x), fp1616(e.event_y));
            }
            Event::Expose(_) => {
                self.ready = true;
            }
            Event::ConfigureNotify(e) => {
                if e.width != 0 && e.height != 0 {
                    self.width = u32::from(e.width);
                    self.height = u32::from(e.height);
                    self.pending_events.push_back(SurfaceEvent::Resized {
                        width: u32::from(e.width),
                        height: u32::from(e.height),
                    });
                }
            }
            Event::ClientMessage(_e) => {
                // WM_DELETE_WINDOW: the WM asks us to close.
                self.pending_events.push_back(SurfaceEvent::CloseRequested);
            }
            _ => {}
        }
    }
}

impl Surface for X11Surface {
    fn backend(&self) -> SurfaceBackend {
        SurfaceBackend::X11NoInput
    }

    fn set_size(&mut self, width: u32, height: u32) -> Result<(), SurfaceError> {
        if width == 0
            || height == 0
            || width > crate::wayland::MAX_DIMENSION
            || height > crate::wayland::MAX_DIMENSION
        {
            return Err(SurfaceError::Unsupported(format!(
                "invalid size {width}x{height}"
            )));
        }
        self.width = width;
        self.height = height;
        let _ = self.conn.configure_window(
            self.window,
            &x11rb::protocol::xproto::ConfigureWindowAux::new()
                .width(width.min(u32::from(u16::MAX)))
                .height(height.min(u32::from(u16::MAX))),
        );
        self.conn
            .flush()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        Ok(())
    }

    fn set_position(&mut self, x: i32, y: i32) -> Result<(), SurfaceError> {
        self.conn
            .configure_window(
                self.window,
                &x11rb::protocol::xproto::ConfigureWindowAux::new()
                    .x(Some(x))
                    .y(Some(y)),
            )
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        self.conn
            .flush()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        Ok(())
    }

    fn position(&self) -> Option<(i32, i32)> {
        let reply = self.conn.get_geometry(self.window).ok()?.reply().ok()?;
        Some((i32::from(reply.x), i32::from(reply.y)))
    }

    fn output_bounds(&self) -> Option<(u32, u32)> {
        Some(self.screen_size)
    }

    fn present(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        stride: u32,
    ) -> Result<(), SurfaceError> {
        // X11 ZPixmap wire layout (verified against Xorg 21.1): depth 24 and
        // 32 both use 4 bytes per pixel (B,G,R,X on little-endian); depth 16
        // uses 2. Rows are always 4-byte aligned, so `width * bpp` is exact.
        let bpp: usize = match self.depth {
            32 | 24 => 4,
            16 => 2,
            other => {
                return Err(SurfaceError::Unsupported(format!(
                    "unsupported window depth {other}"
                )))
            }
        };
        let row_bytes = (width as usize)
            .checked_mul(bpp)
            .ok_or_else(|| SurfaceError::Unsupported("size overflow".into()))?;
        let bytes = (height as usize)
            .checked_mul(row_bytes)
            .ok_or_else(|| SurfaceError::Unsupported("size overflow".into()))?;
        let src_stride = stride as usize;
        let src_bytes = (height as usize)
            .checked_mul(src_stride)
            .ok_or_else(|| SurfaceError::Unsupported("size overflow".into()))?;
        if buffer.len() < src_bytes {
            return Err(SurfaceError::Unsupported("buffer too small".into()));
        }
        let mut x11_buf = vec![0u8; bytes];
        for y in 0..height as usize {
            let src_row = &buffer[y * src_stride..];
            let dst_row = &mut x11_buf[y * row_bytes..];
            for x in 0..width as usize {
                let s = &src_row[x * 4..];
                let d = x * bpp;
                dst_row[d] = s[2]; // B
                dst_row[d + 1] = s[1]; // G
                dst_row[d + 2] = s[0]; // R
                if bpp == 4 {
                    dst_row[d + 3] = 0; // X
                }
            }
        }
        self.conn
            .put_image(
                ImageFormat::Z_PIXMAP,
                self.window,
                self.gc,
                width as u16,
                height as u16,
                0,
                0,
                0,
                self.depth,
                &x11_buf,
            )
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?
            .check()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        self.conn
            .flush()
            .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) -> Result<(), SurfaceError> {
        self.visible = visible;
        if visible {
            self.conn
                .map_window(self.window)
                .map_err(|e| SurfaceError::Protocol(e.to_string()))?
                .check()
                .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
            self.conn
                .flush()
                .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
            self.ready = true;
        } else {
            self.conn
                .unmap_window(self.window)
                .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
            self.conn
                .flush()
                .map_err(|e| SurfaceError::Protocol(e.to_string()))?;
        }
        Ok(())
    }

    fn poll_events(
        &mut self,
        timeout: Option<Duration>,
    ) -> Result<Vec<SurfaceEvent>, SurfaceError> {
        self.drain_x11()?;
        if !self.pending_events.is_empty() {
            return Ok(self.pending_events.drain(..).collect());
        }
        if timeout == Some(Duration::ZERO) {
            return Ok(Vec::new());
        }
        // Wait for readability on the X11 socket, bounded by the timeout.
        let ready = {
            let mut fds = [nix::poll::PollFd::new(
                std::os::fd::AsFd::as_fd(self.conn.stream()),
                nix::poll::PollFlags::POLLIN,
            )];
            let timeout_ms = timeout
                .map(|t| t.as_millis().min(u128::from(u16::MAX)) as u16)
                .unwrap_or(500);
            nix::poll::poll(&mut fds, nix::poll::PollTimeout::from(timeout_ms))
                .map_err(|e| SurfaceError::Io(e.to_string()))?
                > 0
        };
        if ready {
            self.drain_x11()?;
        }
        Ok(self.pending_events.drain(..).collect())
    }

    fn scale_factor(&self) -> f32 {
        1.0
    }

    fn is_ready(&self) -> bool {
        self.ready
    }
}

impl Drop for X11Surface {
    fn drop(&mut self) {
        let _ = self.conn.destroy_window(self.window);
        let _ = self.conn.flush();
    }
}

/// Convert an XInput2 16.16 fixed-point coordinate to physical pixels.
fn fp1616(v: xinput::Fp1616) -> f64 {
    f64::from(v) / 65536.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fp1616_conversion() {
        // 1.5 in 16.16 fixed point is 0x18000.
        assert!((fp1616(0x0001_8000) - 1.5).abs() < 1e-6);
        // 100.25 → 0x6404000.
        assert!((fp1616(0x0064_4000) - 100.25).abs() < 1e-6);
        // Negative coordinates (offscreen) must stay sane.
        assert!(fp1616(-0x0001_0000) < 0.0);
    }
}
