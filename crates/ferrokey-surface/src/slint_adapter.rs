//! The custom Slint platform.
//!
//! Slint explicitly supports replacing its normal backend with your own
//! platform/window adapter. Ferrokey keeps Slint as the UI language and
//! renderer while owning the unusual window semantics itself:
//!
//! ```text
//!                     Slint
//!                      ▲
//!                      │ WindowEvent / Renderer
//!             FerrokeyWindowAdapter
//!               /                \
//!              /                  \
//!      Wayland layer-shell    X11 WM_HINTS.input=false
//!   keyboard_interactivity=none   (+ dock/above/skip-taskbar)
//! ```
//!
//! The exact same `.slint` UI works on every backend — zero compositor
//! policy lives inside the Slint files.

use crate::fallback::NullSurface;
use crate::{PointerButton, Surface, SurfaceEvent};
use slint::platform::software_renderer::{PremultipliedRgbaColor, SoftwareRenderer, TargetPixel};
use slint::platform::{
    Platform, PlatformError, PointerEventButton, Renderer, WindowAdapter, WindowEvent,
};
use slint::{LogicalPosition, LogicalSize, PhysicalSize};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// The window adapter: bridges a [`crate::Surface`] to Slint.
pub struct FerrokeyWindowAdapter {
    pub window: slint::Window,
    renderer: SoftwareRenderer,
    surface: Rc<RefCell<Box<dyn Surface>>>,
    size: Cell<PhysicalSize>,
    scale: Cell<f32>,
    redraw: Cell<bool>,
}

impl FerrokeyWindowAdapter {
    fn new(
        surface: Rc<RefCell<Box<dyn Surface>>>,
        initial_size: PhysicalSize,
        scale: f32,
    ) -> Rc<Self> {
        let adapter: Rc<FerrokeyWindowAdapter> =
            Rc::new_cyclic(|self_weak| FerrokeyWindowAdapter {
                window: slint::Window::new(self_weak.clone() as _),
                renderer: SoftwareRenderer::new(),
                surface,
                size: Cell::new(initial_size),
                scale: Cell::new(scale),
                redraw: Cell::new(true),
            });
        adapter
    }
}

impl WindowAdapter for FerrokeyWindowAdapter {
    fn window(&self) -> &slint::Window {
        &self.window
    }

    fn renderer(&self) -> &dyn Renderer {
        &self.renderer
    }

    fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        self.surface
            .borrow_mut()
            .set_visible(visible)
            .map_err(|e| PlatformError::Other(e.to_string()))?;
        if visible {
            self.redraw.set(true);
        }
        Ok(())
    }

    fn size(&self) -> PhysicalSize {
        self.size.get()
    }

    fn set_size(&self, size: slint::WindowSize) {
        let physical = size.to_physical(self.scale.get());
        self.size.set(physical);
    }

    fn request_redraw(&self) {
        self.redraw.set(true);
    }
}

/// The Ferrokey platform. One window, one surface.
pub struct FerrokeyPlatform {
    surface: Rc<RefCell<Box<dyn Surface>>>,
    adapter: Rc<FerrokeyWindowAdapter>,
    size: Cell<PhysicalSize>,
}

impl FerrokeyPlatform {
    /// Create the platform over the given surface with an initial physical
    /// size (and the surface's current scale factor).
    pub fn new(surface: Box<dyn Surface>, width: u32, height: u32) -> Rc<Self> {
        let surface = Rc::new(RefCell::new(surface));
        let size = PhysicalSize { width, height };
        let scale = surface.borrow().scale_factor();
        let adapter = FerrokeyWindowAdapter::new(surface.clone(), size, scale);
        Rc::new(FerrokeyPlatform {
            surface,
            adapter,
            size: Cell::new(size),
        })
    }

    /// The window adapter (needed by the app to reach the `slint::Window`).
    pub fn adapter(&self) -> Rc<FerrokeyWindowAdapter> {
        self.adapter.clone()
    }

    /// The surface backend in use.
    pub fn backend(&self) -> crate::SurfaceBackend {
        self.surface.borrow().backend()
    }

    /// Resize the OSK. Dispatches a `Resized` event into Slint so the layout
    /// reflows.
    pub fn set_size(&self, width: u32, height: u32) -> Result<(), PlatformError> {
        self.size.set(PhysicalSize { width, height });
        self.adapter.size.set(PhysicalSize { width, height });
        let scale = self.scale();
        self.surface
            .borrow_mut()
            .set_size(width, height)
            .map_err(|e| PlatformError::Other(e.to_string()))?;
        self.window().dispatch_event(WindowEvent::Resized {
            size: LogicalSize::new(width as f32 / scale, height as f32 / scale),
        });
        self.window().request_redraw();
        Ok(())
    }

    /// The OSK's current size (physical px).
    pub fn size(&self) -> PhysicalSize {
        self.size.get()
    }

    /// Move the OSK window (top-left to `(x, y)` in output coordinates).
    /// The no-focus contract is untouched: the surface stays
    /// `keyboard_interactivity = none` / `WM_HINTS.input = False`.
    pub fn set_position(&self, x: i32, y: i32) -> Result<(), PlatformError> {
        self.surface
            .borrow_mut()
            .set_position(x, y)
            .map_err(|e| PlatformError::Other(e.to_string()))
    }

    /// The OSK window's current top-left position (output coordinates),
    /// when the backend can determine it.
    pub fn surface_position(&self) -> Option<(i32, i32)> {
        self.surface.borrow().position()
    }

    /// The output's physical size, when the backend knows it (used to keep
    /// the OSK fully on-screen while moving/resizing).
    pub fn output_bounds(&self) -> Option<(u32, u32)> {
        self.surface.borrow().output_bounds()
    }

    /// Show/hide the OSK.
    pub fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        if visible {
            self.window().show()
        } else {
            self.window().hide()
        }
    }

    /// Process surface events, dispatching them into Slint for visuals and
    /// returning the raw events so the app's pointer bridge can drive key
    /// semantics (rules 18, 85).
    pub fn process_events(
        &self,
        timeout: Option<Duration>,
    ) -> Result<Vec<SurfaceEvent>, PlatformError> {
        let events = self
            .surface
            .borrow_mut()
            .poll_events(timeout)
            .map_err(|e| PlatformError::Other(e.to_string()))?;
        for event in &events {
            self.handle_surface_event(*event);
        }
        Ok(events)
    }

    /// Render the Slint scene into the surface if a redraw was requested.
    pub fn render_if_dirty(&self) -> Result<(), PlatformError> {
        let dirty = self.redraw_requested();
        let surface_ready = self.surface.borrow().is_ready();
        if !dirty || !surface_ready {
            return Ok(());
        }
        let size = self.size.get();
        let width = size.width;
        let height = size.height;
        if width == 0 || height == 0 {
            return Ok(());
        }
        let mut pixels: Vec<PremultipliedRgbaColor> =
            vec![PremultipliedRgbaColor::background(); (width as usize) * (height as usize)];
        self.adapter.renderer.render(&mut pixels, width as usize);
        // PremultipliedRgbaColor memory order R,G,B,A; pack into bytes.
        let mut bytes: Vec<u8> = Vec::with_capacity(pixels.len() * 4);
        for p in &pixels {
            bytes.push(p.red);
            bytes.push(p.green);
            bytes.push(p.blue);
            bytes.push(p.alpha);
        }
        self.surface
            .borrow_mut()
            .present(&bytes, width, height, width * 4)
            .map_err(|e| PlatformError::Other(e.to_string()))?;
        Ok(())
    }

    /// Whether a redraw has been requested and not yet consumed.
    pub fn redraw_requested(&self) -> bool {
        self.adapter.redraw.replace(false)
    }

    /// The `slint::Window` handle (for show/hide and event dispatch).
    pub fn window(&self) -> &slint::Window {
        &self.adapter.window
    }

    pub fn scale(&self) -> f32 {
        self.adapter.scale.get()
    }

    /// The surface, for tests that want to inspect the backend directly.
    pub fn surface(&self) -> &Rc<RefCell<Box<dyn Surface>>> {
        &self.surface
    }

    fn handle_surface_event(&self, event: SurfaceEvent) {
        let scale = self.scale();
        let window = self.window();
        // Keep the platform's size bookkeeping in sync for compositor-driven
        // resizes (the pure translation below cannot do that).
        if let SurfaceEvent::Resized { width, height } = event {
            let physical = PhysicalSize { width, height };
            self.size.set(physical);
            self.adapter.size.set(physical);
        }
        for wevent in surface_event_to_window_events(event, scale) {
            window.dispatch_event(wevent);
        }
        if matches!(event, SurfaceEvent::Resized { .. }) {
            window.request_redraw();
        }
    }
}

/// Translate one surface event into the Slint [`WindowEvent`]s it represents
/// (physical → logical coordinates via `scale`).
///
/// Touch events become pointer events with the left button: Slint's
/// `TouchArea` reacts identically to touch and click, which is precisely the
/// OSK property the compatibility contract requires.
fn surface_event_to_window_events(event: SurfaceEvent, scale: f32) -> Vec<WindowEvent> {
    let pos = |x: f64, y: f64| {
        LogicalPosition::new((x / f64::from(scale)) as f32, (y / f64::from(scale)) as f32)
    };
    match event {
        SurfaceEvent::PointerMoved { x, y } | SurfaceEvent::TouchMoved { x, y } => {
            vec![WindowEvent::PointerMoved {
                position: pos(x, y),
            }]
        }
        SurfaceEvent::PointerPressed { x, y, button } => vec![WindowEvent::PointerPressed {
            position: pos(x, y),
            button: to_slint_button(button),
        }],
        SurfaceEvent::PointerReleased { x, y, button } => vec![WindowEvent::PointerReleased {
            position: pos(x, y),
            button: to_slint_button(button),
        }],
        SurfaceEvent::PointerLeft | SurfaceEvent::TouchCancelled => {
            vec![WindowEvent::PointerExited]
        }
        SurfaceEvent::TouchPressed { x, y } => vec![WindowEvent::PointerPressed {
            position: pos(x, y),
            button: PointerEventButton::Left,
        }],
        SurfaceEvent::TouchReleased { x, y } => vec![WindowEvent::PointerReleased {
            position: pos(x, y),
            button: PointerEventButton::Left,
        }],
        SurfaceEvent::Resized { width, height } => {
            vec![WindowEvent::Resized {
                size: LogicalSize::new(width as f32 / scale, height as f32 / scale),
            }]
        }
        SurfaceEvent::CloseRequested => vec![WindowEvent::CloseRequested],
    }
}

fn to_slint_button(button: PointerButton) -> PointerEventButton {
    match button {
        PointerButton::Left => PointerEventButton::Left,
        PointerButton::Middle => PointerEventButton::Middle,
        PointerButton::Right => PointerEventButton::Right,
    }
}

impl Platform for FerrokeyPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.adapter.clone())
    }
}

/// A `Box<dyn Platform>` handle over a shared [`FerrokeyPlatform`] (newtype:
/// the orphan rule forbids implementing the foreign `Platform` trait for the
/// foreign `Rc` type).
pub struct PlatformHandle(pub Rc<FerrokeyPlatform>);

impl Platform for PlatformHandle {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        self.0.create_window_adapter()
    }
}

/// Convenience: create a platform over a null surface (headless/degraded).
pub fn null_platform(width: u32, height: u32) -> Rc<FerrokeyPlatform> {
    FerrokeyPlatform::new(Box::new(NullSurface::new()), width, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_platform_has_expected_backend() {
        let platform = null_platform(320, 200);
        assert_eq!(platform.backend(), crate::SurfaceBackend::None);
        assert!((platform.scale() - 1.0).abs() < f32::EPSILON);
        // Events on a null surface are always empty.
        platform.process_events(Some(Duration::ZERO)).unwrap();
    }

    #[test]
    fn window_adapter_round_trip() {
        let platform = null_platform(320, 200);
        let adapter = platform.adapter();
        let size = adapter.size();
        assert_eq!(size.width, 320);
        assert_eq!(size.height, 200);
        // request_redraw must flip the flag.
        assert!(platform.redraw_requested());
        adapter.request_redraw();
        assert!(platform.redraw_requested());
        assert!(!platform.redraw_requested());
    }

    #[test]
    fn touch_maps_to_left_pointer_events() {
        let events =
            surface_event_to_window_events(SurfaceEvent::TouchPressed { x: 80.0, y: 40.0 }, 2.0);
        assert_eq!(
            events,
            vec![WindowEvent::PointerPressed {
                position: LogicalPosition::new(40.0, 20.0),
                button: PointerEventButton::Left,
            }]
        );
        let events =
            surface_event_to_window_events(SurfaceEvent::TouchMoved { x: 160.0, y: 80.0 }, 2.0);
        assert_eq!(
            events,
            vec![WindowEvent::PointerMoved {
                position: LogicalPosition::new(80.0, 40.0),
            }]
        );
        let events =
            surface_event_to_window_events(SurfaceEvent::TouchReleased { x: 90.0, y: 50.0 }, 2.0);
        assert_eq!(
            events,
            vec![WindowEvent::PointerReleased {
                position: LogicalPosition::new(45.0, 25.0),
                button: PointerEventButton::Left,
            }]
        );
        let events = surface_event_to_window_events(SurfaceEvent::TouchCancelled, 1.0);
        assert_eq!(events, vec![WindowEvent::PointerExited]);
    }

    #[test]
    fn pointer_buttons_map_preserving_button() {
        let events = surface_event_to_window_events(
            SurfaceEvent::PointerPressed {
                x: 10.0,
                y: 20.0,
                button: crate::PointerButton::Right,
            },
            1.0,
        );
        assert_eq!(
            events,
            vec![WindowEvent::PointerPressed {
                position: LogicalPosition::new(10.0, 20.0),
                button: PointerEventButton::Right,
            }]
        );
    }

    #[test]
    fn scale_affects_coordinates() {
        let events =
            surface_event_to_window_events(SurfaceEvent::TouchPressed { x: 200.0, y: 100.0 }, 4.0);
        assert_eq!(
            events,
            vec![WindowEvent::PointerPressed {
                position: LogicalPosition::new(50.0, 25.0),
                button: PointerEventButton::Left,
            }]
        );
    }
}
