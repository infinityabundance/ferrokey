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

    /// Show/hide the OSK.
    pub fn set_visible(&self, visible: bool) -> Result<(), PlatformError> {
        if visible {
            self.window().show()
        } else {
            self.window().hide()
        }
    }

    /// Process surface events, dispatching them into Slint.
    pub fn process_events(&self, timeout: Option<Duration>) -> Result<(), PlatformError> {
        let events = self
            .surface
            .borrow_mut()
            .poll_events(timeout)
            .map_err(|e| PlatformError::Other(e.to_string()))?;
        for event in events {
            self.handle_surface_event(event);
        }
        Ok(())
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
        match event {
            SurfaceEvent::PointerMoved { x, y } => {
                window.dispatch_event(WindowEvent::PointerMoved {
                    position: LogicalPosition::new(
                        (x / f64::from(scale)) as f32,
                        (y / f64::from(scale)) as f32,
                    ),
                });
            }
            SurfaceEvent::PointerPressed { x, y, button } => {
                window.dispatch_event(WindowEvent::PointerPressed {
                    position: LogicalPosition::new(
                        (x / f64::from(scale)) as f32,
                        (y / f64::from(scale)) as f32,
                    ),
                    button: to_slint_button(button),
                });
            }
            SurfaceEvent::PointerReleased { x, y, button } => {
                window.dispatch_event(WindowEvent::PointerReleased {
                    position: LogicalPosition::new(
                        (x / f64::from(scale)) as f32,
                        (y / f64::from(scale)) as f32,
                    ),
                    button: to_slint_button(button),
                });
            }
            SurfaceEvent::PointerLeft => {
                window.dispatch_event(WindowEvent::PointerExited);
            }
            SurfaceEvent::Resized { width, height } => {
                let physical = PhysicalSize { width, height };
                self.size.set(physical);
                self.adapter.size.set(physical);
                window.dispatch_event(WindowEvent::Resized {
                    size: LogicalSize::new(width as f32 / scale, height as f32 / scale),
                });
                window.request_redraw();
            }
            SurfaceEvent::CloseRequested => {
                window.dispatch_event(WindowEvent::CloseRequested);
            }
        }
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
}
