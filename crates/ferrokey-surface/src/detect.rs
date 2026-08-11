//! Runtime capability detection.
//!
//! Ferrokey probes **capabilities**, not compositor names:
//!
//! ```text
//! Is WAYLAND_DISPLAY available?
//!   ├─ yes ── zwlr_layer_shell_v1 advertised?
//!   │           ├─ YES → WaylandLayerShell (ideal)
//!   │           └─ no  ── XWayland/X11 available?
//!   │                   ├─ YES → X11NoInput (XWayland)
//!   │                   └─ no  → WaylandDegraded (explicit warning)
//!   └─ no ── DISPLAY available?
//!               ├─ yes → X11NoInput (native Xorg)
//!               └─ no  → None (headless)
//! ```

use crate::SurfaceBackend;
use std::path::Path;

/// The result of probing the session.
#[derive(Debug, Clone)]
pub struct Detection {
    pub backend: SurfaceBackend,
    /// Human-readable summary of what was found.
    pub detail: String,
    /// The X11 display string to use for the X11 backend, if any.
    pub x11_display: Option<String>,
}

/// Whether a Wayland display appears to be available (env probe only).
pub fn wayland_env_present() -> bool {
    if let Ok(display) = std::env::var("WAYLAND_DISPLAY") {
        if !display.is_empty() {
            // The socket may live in XDG_RUNTIME_DIR (or be an absolute path).
            if Path::new(&display).is_absolute() {
                return Path::new(&display).exists();
            }
            if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
                let sock = Path::new(&runtime_dir).join(&display);
                if sock.exists() {
                    return true;
                }
            }
            return true; // compositor may still create it; let connect probe decide
        }
    }
    false
}

/// Probe the session and choose the best surface backend.
pub fn detect() -> Detection {
    if wayland_env_present() {
        match crate::wayland::probe_globals() {
            Ok(has_layer_shell) if has_layer_shell => Detection {
                backend: SurfaceBackend::WaylandLayerShell,
                detail: "Wayland session with zwlr_layer_shell_v1".into(),
                x11_display: None,
            },
            Ok(_) => {
                // Wayland but no layer-shell. Try XWayland, else degrade.
                let x11 = std::env::var("DISPLAY").ok().filter(|d| !d.is_empty());
                match x11 {
                    Some(display) => Detection {
                        backend: SurfaceBackend::X11NoInput,
                        detail: format!(
                            "Wayland session without layer-shell; falling back to X11 surface on {display} (XWayland)"
                        ),
                        x11_display: Some(display),
                    },
                    None => Detection {
                        backend: SurfaceBackend::WaylandDegraded,
                        detail: "Wayland session without layer-shell and without XWayland; degraded mode".into(),
                        x11_display: None,
                    },
                }
            }
            Err(e) => Detection {
                backend: SurfaceBackend::WaylandDegraded,
                detail: format!("Wayland connection failed ({e}); degraded mode"),
                x11_display: None,
            },
        }
    } else if let Ok(display) = std::env::var("DISPLAY") {
        if !display.is_empty() {
            Detection {
                backend: SurfaceBackend::X11NoInput,
                detail: format!("X11 session on {display}"),
                x11_display: Some(display),
            }
        } else {
            Detection {
                backend: SurfaceBackend::None,
                detail: "no display server detected".into(),
                x11_display: None,
            }
        }
    } else {
        Detection {
            backend: SurfaceBackend::None,
            detail: "no display server detected (headless)".into(),
            x11_display: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_display_env_detects_none() {
        // Simulate a clean environment: no WAYLAND_DISPLAY, no DISPLAY.
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("DISPLAY");
        std::env::remove_var("XDG_RUNTIME_DIR");
        let d = detect();
        assert_eq!(d.backend, SurfaceBackend::None);
    }
}
