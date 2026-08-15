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
//!
//! The decision itself ([`decide`]) is a **pure function of observed facts**
//! ([`SessionProbe`]): the court can therefore assert the selected backend
//! deterministically for every fixture combination (Wayland±layer-shell±
//! XWayland, X11-only, headless — §65/§66 of the addendum), and the runtime
//! path ([`probe_session`]) is reduced to gathering those facts. Every
//! non-ideal outcome carries a human-readable *reason* in `Detection.detail`,
//! which the UI shows and the app logs at startup.

use crate::SurfaceBackend;
use std::path::Path;

/// The result of probing the session.
#[derive(Debug, Clone)]
pub struct Detection {
    pub backend: SurfaceBackend,
    /// Human-readable summary of what was found (and, for a degraded or
    /// fallen-back backend, *why* — the rejection reason, §66).
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

/// Every observed fact a deterministic backend decision can depend on.
///
/// The fields are gathered from the environment / the session by
/// [`probe_session`]; [`decide`] maps the combination to a backend. Keeping
/// the decision pure makes the §65 selection policy unit-testable over the
/// full fixture matrix without touching a real compositor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProbe {
    /// `WAYLAND_DISPLAY` env appears usable.
    pub wayland_env: bool,
    /// Connecting to the compositor and reading the global list: `Ok(true)`
    /// when `zwlr_layer_shell_v1` is advertised, `Ok(false)` when the
    /// session is Wayland without layer-shell, `Err(reason)` when the
    /// connection/protocol failed. `None` when no Wayland env was present
    /// (nothing was probed).
    pub wayland_globals: Option<Result<bool, String>>,
    /// The non-empty `DISPLAY` env value, if any.
    pub x11_display: Option<String>,
}

/// The deterministic selection policy (§65): pure, exhaustive, and identical
/// to the runtime path.
pub fn decide(probe: &SessionProbe) -> Detection {
    if probe.wayland_env {
        match &probe.wayland_globals {
            Some(Ok(true)) => Detection {
                backend: SurfaceBackend::WaylandLayerShell,
                detail: "Wayland session with zwlr_layer_shell_v1".into(),
                x11_display: None,
            },
            Some(Ok(false)) => {
                // Wayland but no layer-shell. Try XWayland, else degrade.
                match &probe.x11_display {
                    Some(display) => Detection {
                        backend: SurfaceBackend::X11NoInput,
                        detail: format!(
                            "Wayland session without layer-shell; falling back to X11 surface on {display} (XWayland)"
                        ),
                        x11_display: Some(display.clone()),
                    },
                    None => Detection {
                        backend: SurfaceBackend::WaylandDegraded,
                        detail: "Wayland session without layer-shell and without XWayland; degraded mode".into(),
                        x11_display: None,
                    },
                }
            }
            Some(Err(e)) => Detection {
                backend: SurfaceBackend::WaylandDegraded,
                detail: format!("Wayland connection failed ({e}); degraded mode"),
                x11_display: None,
            },
            // Wayland env was present but the globals were never probed —
            // treat as degraded rather than guessing (never claim a
            // capability that was not observed).
            None => Detection {
                backend: SurfaceBackend::WaylandDegraded,
                detail: "Wayland session present but globals not probed; degraded mode".into(),
                x11_display: None,
            },
        }
    } else if let Some(display) = &probe.x11_display {
        Detection {
            backend: SurfaceBackend::X11NoInput,
            detail: format!("X11 session on {display}"),
            x11_display: Some(display.clone()),
        }
    } else {
        Detection {
            backend: SurfaceBackend::None,
            detail: "no display server detected (headless)".into(),
            x11_display: None,
        }
    }
}

/// Gather the session facts (environment + a real compositor probe).
pub fn probe_session() -> SessionProbe {
    let wayland_env = wayland_env_present();
    let wayland_globals = if wayland_env {
        #[cfg(feature = "wayland")]
        {
            Some(crate::wayland::probe_globals().map_err(|e| e.to_string()))
        }
        #[cfg(not(feature = "wayland"))]
        {
            Some(Err(
                "ferrokey-surface built without the wayland feature".into()
            ))
        }
    } else {
        None
    };
    let x11_display = std::env::var("DISPLAY").ok().filter(|d| !d.is_empty());
    SessionProbe {
        wayland_env,
        wayland_globals,
        x11_display,
    }
}

/// Probe the session and choose the best surface backend.
pub fn detect() -> Detection {
    decide(&probe_session())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe(wayland_env: bool, layer_shell: Option<bool>, display: Option<&str>) -> SessionProbe {
        SessionProbe {
            wayland_env,
            wayland_globals: layer_shell.map(Ok),
            x11_display: display.map(str::to_string),
        }
    }

    // ── the §65 decision matrix: every fixture combination ────────────────

    #[test]
    fn headless_detects_none() {
        let d = decide(&probe(false, None, None));
        assert_eq!(d.backend, SurfaceBackend::None);
        assert!(d.detail.contains("headless"));
    }

    #[test]
    fn x11_only_detects_x11_no_input() {
        let d = decide(&probe(false, None, Some(":0")));
        assert_eq!(d.backend, SurfaceBackend::X11NoInput);
        assert_eq!(d.x11_display.as_deref(), Some(":0"));
        assert!(d.detail.contains(":0"));
    }

    #[test]
    fn wayland_with_layer_shell_wins_over_x11() {
        // Even with a DISPLAY present, layer-shell is preferred.
        let d = decide(&probe(true, Some(true), Some(":0")));
        assert_eq!(d.backend, SurfaceBackend::WaylandLayerShell);
        assert_eq!(d.x11_display, None);
    }

    #[test]
    fn wayland_without_layer_shell_falls_back_to_xwayland() {
        let d = decide(&probe(true, Some(false), Some(":0")));
        assert_eq!(d.backend, SurfaceBackend::X11NoInput);
        assert_eq!(d.x11_display.as_deref(), Some(":0"));
        assert!(d.detail.contains("without layer-shell"), "{}", d.detail);
        assert!(d.detail.contains("XWayland"), "{}", d.detail);
    }

    #[test]
    fn wayland_without_layer_shell_without_x11_degrades() {
        let d = decide(&probe(true, Some(false), None));
        assert_eq!(d.backend, SurfaceBackend::WaylandDegraded);
        assert!(!d.backend.preserves_focus());
        assert!(d.detail.contains("degraded"), "{}", d.detail);
    }

    #[test]
    fn wayland_connection_failure_degrades_with_reason() {
        let d = decide(&SessionProbe {
            wayland_env: true,
            wayland_globals: Some(Err("no such socket: /run/user/1000/nope".into())),
            x11_display: Some(":0".into()),
        });
        assert_eq!(d.backend, SurfaceBackend::WaylandDegraded);
        assert!(d.detail.contains("connection failed"), "{}", d.detail);
        assert!(d.detail.contains("no such socket"), "{}", d.detail);
    }

    #[test]
    fn wayland_env_without_probe_never_claims_a_capability() {
        // Defensive: an unprobed session must not be reported as layer-shell.
        let d = decide(&probe(true, None, Some(":0")));
        assert_eq!(d.backend, SurfaceBackend::WaylandDegraded);
    }

    #[test]
    fn empty_display_is_headless() {
        // DISPLAY="" counts as no display (the X11 branch filters empties).
        let d = decide(&probe(false, None, None));
        assert_eq!(d.backend, SurfaceBackend::None);
    }

    // ── the runtime gatherer (environment-driven; single test so the
    //    env mutations cannot race a sibling test) ─────────────────────────

    #[test]
    fn env_probe_and_detect_agree() {
        // Simulate a clean environment: no WAYLAND_DISPLAY, no DISPLAY.
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("DISPLAY");
        std::env::remove_var("XDG_RUNTIME_DIR");
        let d = detect();
        assert_eq!(d.backend, SurfaceBackend::None);

        // A bare X11 display must be carried through the gatherer + decision.
        std::env::set_var("DISPLAY", ":77");
        let p = probe_session();
        assert!(!p.wayland_env);
        assert_eq!(p.x11_display.as_deref(), Some(":77"));
        let d = detect();
        assert_eq!(d.backend, SurfaceBackend::X11NoInput);
        assert_eq!(d.x11_display.as_deref(), Some(":77"));
        std::env::remove_var("DISPLAY");
    }
}
