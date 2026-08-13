//! Shared state-reporting socket for court target applications.
//!
//! Every `ferrokey-test-target-*` exposes machine-readable state on a Unix
//! socket so the compatibility court has a precise oracle — never
//! screenshots alone. Events are JSON lines broadcast to every connected
//! client:
//!
//! ```json
//! {"event":"focus","focused":true}
//! {"event":"text","text":"hello"}
//! {"event":"key","code":30,"down":true}
//! ```
//!
//! The court reads these lines and asserts on them.
//!
//! Every newly-connected client immediately receives a *state snapshot*
//! (`Ready`, plus the current focus/text state). Courts start the recorder
//! before the target and may connect at any point; a one-shot broadcast would
//! race with early events (e.g. the first `FocusIn` arriving ~0.2s after
//! map), so new clients must not miss state that already happened.

use serde::Serialize;
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};
use std::thread;

/// A machine-readable event from a target application.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum TargetEvent {
    /// Keyboard focus entered/left the target window.
    Focus { focused: bool },
    /// Focus-change detail (X11 `Notify*` mode) — why the focus changed;
    /// courts use it to attribute focus flickers.
    FocusDetail { mode: String, detail: String },
    /// The visible text of the target's input field.
    Text { text: String },
    /// A raw key event observed by the target.
    Key { code: u32, down: bool },
    /// A printable character was delivered (widget-level, after keymap).
    Char { ch: String },
    /// The target is ready and listening.
    Ready,
}

/// The current state of a target, replayed to every new client.
#[derive(Debug, Default)]
struct CurrentState {
    focused: Option<bool>,
    text: Option<String>,
}

/// Broadcasts [`TargetEvent`]s to every connected client.
pub struct Reporter {
    listener: Arc<UnixListener>,
    clients: Arc<Mutex<Vec<UnixStream>>>,
    state: Arc<Mutex<CurrentState>>,
}

impl Reporter {
    /// Bind the report socket. Path: `$TARGET_SOCKET` or
    /// `/tmp/ferrokey-test-target.sock`. A stale socket is removed first.
    pub fn bind() -> std::io::Result<Self> {
        let path = std::env::var("TARGET_SOCKET")
            .unwrap_or_else(|_| "/tmp/ferrokey-test-target.sock".into());
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        log::info!("target reporter listening on {path}");
        Ok(Reporter {
            listener: Arc::new(listener),
            clients: Arc::new(Mutex::new(Vec::new())),
            state: Arc::new(Mutex::new(CurrentState::default())),
        })
    }

    /// Spawn the accept loop. Call once; the loop runs forever. Every new
    /// client immediately receives a state snapshot (the `Ready` marker plus
    /// the current focus/text state), so courts that connect after startup
    /// never miss events that already happened.
    pub fn spawn_accept_loop(&self) {
        let listener = self.listener.clone();
        let clients = self.clients.clone();
        let state = self.state.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(mut stream) => {
                        let _ = stream.set_nonblocking(true);
                        // Snapshot for the new client: Ready first, then the
                        // current focus/text state if any is known.
                        let mut buf = serde_json::to_string(&TargetEvent::Ready)
                            .unwrap_or_else(|_| "{}".into())
                            .into_bytes();
                        buf.push(b'\n');
                        let _ = stream.write_all(&buf);
                        if let Ok(cur) = state.lock() {
                            if let Some(focused) = cur.focused {
                                let line = serde_json::to_string(&TargetEvent::Focus { focused })
                                    .unwrap_or_default();
                                let _ = stream.write_all(format!("{line}\n").as_bytes());
                            }
                            if let Some(text) = &cur.text {
                                let line = serde_json::to_string(&TargetEvent::Text {
                                    text: text.clone(),
                                })
                                .unwrap_or_default();
                                let _ = stream.write_all(format!("{line}\n").as_bytes());
                            }
                        }
                        clients.lock().unwrap().push(stream);
                    }
                    Err(e) => log::warn!("target reporter accept failed: {e}"),
                }
            }
        });
    }

    /// Broadcast an event to all connected clients.
    pub fn report(&self, event: TargetEvent) {
        // Track the current state so late-connecting clients get a snapshot.
        match &event {
            TargetEvent::Focus { focused } => {
                if let Ok(mut cur) = self.state.lock() {
                    cur.focused = Some(*focused);
                }
            }
            TargetEvent::Text { text } => {
                if let Ok(mut cur) = self.state.lock() {
                    cur.text = Some(text.clone());
                }
            }
            _ => {}
        }
        let line = match serde_json::to_string(&event) {
            Ok(line) => line,
            Err(e) => {
                log::warn!("report serialization failed: {e}");
                return;
            }
        };
        let mut clients = self.clients.lock().unwrap();
        clients.retain_mut(|stream| {
            let mut buf = line.as_bytes().to_vec();
            buf.push(b'\n');
            stream.write_all(&buf).is_ok()
        });
    }

    /// Convenience: report text change.
    pub fn text(&self, text: &str) {
        self.report(TargetEvent::Text { text: text.into() });
    }

    pub fn focus(&self, focused: bool) {
        self.report(TargetEvent::Focus { focused });
    }

    /// Report an X11 focus-change with its cause (mode + detail).
    pub fn focus_detail(&self, mode: &str, detail: &str) {
        self.report(TargetEvent::FocusDetail {
            mode: mode.into(),
            detail: detail.into(),
        });
    }

    pub fn key(&self, code: u32, down: bool) {
        self.report(TargetEvent::Key { code, down });
    }

    pub fn ch(&self, ch: char) {
        self.report(TargetEvent::Char { ch: ch.to_string() });
    }

    pub fn ready(&self) {
        self.report(TargetEvent::Ready);
    }
}
