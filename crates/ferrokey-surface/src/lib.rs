//! # ferrokey-surface
//!
//! Ferrokey's window-system integration — and the piece that makes the
//! project interesting: **Slint renders and hit-tests; Ferrokey owns the
//! window semantics.**
//!
//! The crate implements a custom Slint platform (`slint::platform::Platform`
//! and `WindowAdapter`) on top of raw Wayland and X11 surfaces, so the
//! exact same `.slint` UI works on every backend:
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
//! Backends are selected by **capability detection at runtime** — never by
//! `if compositor == "sway"`-style name matching.

#![forbid(unsafe_code)]

pub mod detect;
pub mod fallback;
pub mod slint_adapter;
#[cfg(feature = "wayland")]
pub mod wayland;
#[cfg(feature = "x11")]
pub mod x11;

use std::time::Duration;

/// Which surface backend is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceBackend {
    /// Native Wayland with `zwlr_layer_shell_v1`: overlay layer, anchored
    /// bottom, `keyboard_interactivity = none` — the compositor guarantees
    /// the target keeps keyboard focus while the OSK receives pointer input.
    WaylandLayerShell,
    /// X11 (native Xorg or XWayland) with ICCCM `WM_HINTS.input = False`.
    X11NoInput,
    /// Wayland session present but no layer-shell: only a degraded (non-focus
    /// preserving) surface is possible. Ferrokey reports this explicitly.
    WaylandDegraded,
    /// No display at all (headless / no compositor). The UI can still run for
    /// testing with a null surface.
    None,
}

impl SurfaceBackend {
    pub const fn name(self) -> &'static str {
        match self {
            SurfaceBackend::WaylandLayerShell => "wayland-layer-shell",
            SurfaceBackend::X11NoInput => "x11-no-input",
            SurfaceBackend::WaylandDegraded => "wayland-degraded",
            SurfaceBackend::None => "none",
        }
    }

    /// Whether this backend guarantees the focus-preservation invariant
    /// (`focus_before == focus_after`).
    pub const fn preserves_focus(self) -> bool {
        matches!(
            self,
            SurfaceBackend::WaylandLayerShell | SurfaceBackend::X11NoInput
        )
    }
}

/// Pointer/input events produced by a surface, in *physical* pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SurfaceEvent {
    PointerMoved {
        x: f64,
        y: f64,
    },
    PointerPressed {
        x: f64,
        y: f64,
        button: PointerButton,
    },
    PointerReleased {
        x: f64,
        y: f64,
        button: PointerButton,
    },
    PointerLeft,
    /// The surface was resized (compositor-driven).
    Resized {
        width: u32,
        height: u32,
    },
    /// The compositor asked the surface to go away (X11 WM_DELETE etc.).
    CloseRequested,
}

/// Pointer buttons Ferrokey translates to Slint events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
}

/// Errors from surface operations.
#[derive(Debug, thiserror::Error)]
pub enum SurfaceError {
    #[error("cannot connect to display: {0}")]
    Connect(String),
    #[error("required protocol object missing: {0}")]
    Missing(String),
    #[error("surface protocol error: {0}")]
    Protocol(String),
    #[error("I/O error: {0}")]
    Io(String),
    #[error("unsupported in this backend: {0}")]
    Unsupported(String),
}

impl From<std::io::Error> for SurfaceError {
    fn from(e: std::io::Error) -> Self {
        SurfaceError::Io(e.to_string())
    }
}

/// A renderable, pointer-interactive, keyboard-focus-free surface.
///
/// Implementations: Wayland layer-shell, X11 no-input, and a null fallback.
pub trait Surface {
    /// The backend in use.
    fn backend(&self) -> SurfaceBackend;

    /// Resize the surface.
    fn set_size(&mut self, width: u32, height: u32) -> Result<(), SurfaceError>;

    /// Present a rendered frame. `buffer` is `width * height`
    /// [`slint::platform::software_renderer::PremultipliedRgbaColor`]s
    /// (memory order R,G,B,A) with the given stride in pixels.
    fn present(
        &mut self,
        buffer: &[u8],
        width: u32,
        height: u32,
        stride: u32,
    ) -> Result<(), SurfaceError>;

    fn set_visible(&mut self, visible: bool) -> Result<(), SurfaceError>;

    /// Wait up to `timeout` for surface events. `timeout = 0` polls without
    /// blocking. Returns all events accumulated since the last call.
    fn poll_events(&mut self, timeout: Option<Duration>)
        -> Result<Vec<SurfaceEvent>, SurfaceError>;

    /// The current scale factor (physical pixels per logical pixel).
    fn scale_factor(&self) -> f32;

    /// Whether the surface has been configured by the compositor and is ready
    /// to present frames.
    fn is_ready(&self) -> bool;
}
