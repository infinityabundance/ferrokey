//! The null surface fallback.
//!
//! Used when no focus-preserving backend is available (headless, or Wayland
//! without layer-shell and without XWayland). It is deliberately **not** a
//! silent downgrade: the UI surfaces a clear degraded-mode warning, and the
//! null surface simply never presents frames or delivers input. This keeps
//! Ferrokey runnable (and testable) without ever pretending to preserve
//! focus it cannot guarantee.

use crate::{Surface, SurfaceBackend, SurfaceError, SurfaceEvent};
use std::time::Duration;

/// A surface that does nothing.
#[derive(Debug, Default)]
pub struct NullSurface {
    width: u32,
    height: u32,
    visible: bool,
    ready: bool,
}

impl NullSurface {
    pub fn new() -> Self {
        NullSurface {
            width: 0,
            height: 0,
            visible: false,
            ready: false,
        }
    }
}

impl Surface for NullSurface {
    fn backend(&self) -> SurfaceBackend {
        SurfaceBackend::None
    }

    fn set_size(&mut self, width: u32, height: u32) -> Result<(), SurfaceError> {
        self.width = width;
        self.height = height;
        Ok(())
    }

    fn set_position(&mut self, _x: i32, _y: i32) -> Result<(), SurfaceError> {
        // No display: nothing to move.
        Ok(())
    }

    fn position(&self) -> Option<(i32, i32)> {
        None
    }

    fn output_bounds(&self) -> Option<(u32, u32)> {
        None
    }

    fn present(
        &mut self,
        _buffer: &[u8],
        _width: u32,
        _height: u32,
        _stride: u32,
    ) -> Result<(), SurfaceError> {
        // No display: the frame is discarded.
        self.ready = self.visible;
        Ok(())
    }

    fn set_visible(&mut self, visible: bool) -> Result<(), SurfaceError> {
        self.visible = visible;
        if visible && self.width > 0 && self.height > 0 {
            self.ready = true;
        }
        Ok(())
    }

    fn poll_events(
        &mut self,
        _timeout: Option<Duration>,
    ) -> Result<Vec<SurfaceEvent>, SurfaceError> {
        Ok(Vec::new())
    }

    fn scale_factor(&self) -> f32 {
        1.0
    }

    fn is_ready(&self) -> bool {
        self.ready
    }
}
