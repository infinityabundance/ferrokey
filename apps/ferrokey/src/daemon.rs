//! The daemon link: a [`ferrokey_core::KeySink`] that reconnects.
//!
//! The UI is fully unprivileged. All key events flow to `ferrokeyd` over the
//! authenticated Unix socket; this type owns the connection lifecycle:
//!
//! * connect + handshake (HELLO / CREATE_KEYBOARD) with backoff
//! * automatic reconnect when the daemon disappears (daemon restart court)
//! * reports connection state to the status line
//!
//! Recovery contract: when the link loses the daemon, the core driver's
//! state is released locally (the caller calls `emergency_release`); the
//! daemon side releases its own device on disconnect.

use ferrokey_core::{KeySink, PhysicalKey, SinkError};
use ferrokey_protocol::{Client, ErrorCode, Message, PROTOCOL_VERSION};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// State reported to the UI status line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    Disconnected,
    Connecting,
    Connected,
    Rejected(String),
}

impl LinkState {
    pub fn label(self) -> String {
        match self {
            LinkState::Disconnected => "daemon offline — waiting".into(),
            LinkState::Connecting => "connecting to ferrokeyd…".into(),
            LinkState::Connected => String::new(),
            LinkState::Rejected(msg) => format!("ferrokeyd rejected connection: {msg}"),
        }
    }
}

/// A reconnecting protocol client implementing [`KeySink`].
pub struct DaemonLink {
    path: PathBuf,
    client: Option<Client>,
    state: LinkState,
    next_attempt: Instant,
    backoff: Duration,
    last_error: Option<String>,
}

impl DaemonLink {
    pub fn new(path: PathBuf) -> Self {
        DaemonLink {
            path,
            client: None,
            state: LinkState::Disconnected,
            next_attempt: Instant::now(),
            backoff: Duration::from_millis(100),
            last_error: None,
        }
    }

    pub fn state(&self) -> LinkState {
        self.state.clone()
    }

    /// Try to (re)connect if we are due.
    pub fn poll_connect(&mut self) {
        if self.client.is_some() || Instant::now() < self.next_attempt {
            return;
        }
        self.state = LinkState::Connecting;
        match Client::connect(&self.path) {
            Ok(mut client) => {
                let handshake = client
                    .send(&Message::Hello {
                        version: PROTOCOL_VERSION,
                        client_name: format!("ferrokey-ui/{}", env!("CARGO_PKG_VERSION")),
                    })
                    .and_then(|()| client.send(&Message::CreateKeyboard));
                match handshake {
                    Ok(()) => {
                        client.set_nonblocking(true).ok();
                        self.client = Some(client);
                        self.state = LinkState::Connected;
                        self.backoff = Duration::from_millis(100);
                        log::info!("connected to ferrokeyd at {}", self.path.display());
                    }
                    Err(e) => {
                        self.last_error = Some(format!("handshake failed: {e}"));
                        self.state = LinkState::Rejected(self.last_error.clone().unwrap());
                        self.schedule_retry();
                    }
                }
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
                self.state = LinkState::Disconnected;
                self.schedule_retry();
            }
        }
    }

    fn schedule_retry(&mut self) {
        self.next_attempt = Instant::now() + self.backoff;
        self.backoff = (self.backoff * 2).min(Duration::from_secs(5));
    }

    /// Read pending server messages (Pong, Error). Returns `Ok(true)` if the
    /// connection is healthy, `Ok(false)` if it should be re-established.
    pub fn poll_server(&mut self) -> Result<(), ()> {
        let Some(client) = self.client.as_mut() else {
            return Err(());
        };
        match client.read_available() {
            Ok(messages) => {
                for msg in messages {
                    if let Message::Error(code, detail) = msg {
                        log::warn!("server error {code:?}: {detail}");
                        self.last_error = Some(format!("{code:?}: {detail}"));
                        if code == ErrorCode::Unauthorized {
                            self.state = LinkState::Rejected(self.last_error.clone().unwrap());
                        }
                    }
                }
                Ok(())
            }
            Err(e) => {
                log::warn!("daemon connection lost: {e}");
                self.dropped();
                Err(())
            }
        }
    }

    /// The connection was lost: drop the client and schedule a reconnect.
    pub fn dropped(&mut self) {
        self.client = None;
        self.state = LinkState::Disconnected;
        self.schedule_retry();
    }

    pub fn is_connected(&self) -> bool {
        self.client.is_some()
    }

    /// Send a heartbeat ping.
    pub fn ping(&mut self) -> Result<(), SinkError> {
        self.send_message(Message::Ping(0))
    }
}

impl KeySink for DaemonLink {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.send_message(Message::KeyDown(key.linux_code() as u16))
    }

    fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.send_message(Message::KeyUp(key.linux_code() as u16))
    }

    fn key_repeat(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.send_message(Message::KeyRepeat(key.linux_code() as u16))
    }

    fn release_all(&mut self) -> Result<(), SinkError> {
        self.send_message(Message::ReleaseAll)
    }
}

/// A `KeySink` over a shared [`DaemonLink`] (newtype: the orphan rule forbids
/// implementing a foreign trait for `Rc<RefCell<_>>` directly).
pub struct DaemonLinkSink(pub std::rc::Rc<std::cell::RefCell<DaemonLink>>);

impl KeySink for DaemonLinkSink {
    fn key_down(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.0.borrow_mut().key_down(key)
    }

    fn key_up(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.0.borrow_mut().key_up(key)
    }

    fn key_repeat(&mut self, key: PhysicalKey) -> Result<(), SinkError> {
        self.0.borrow_mut().key_repeat(key)
    }

    fn release_all(&mut self) -> Result<(), SinkError> {
        self.0.borrow_mut().release_all()
    }
}

impl DaemonLink {
    fn send_message(&mut self, msg: Message) -> Result<(), SinkError> {
        self.poll_connect();
        let Some(client) = self.client.as_mut() else {
            return Err(SinkError::from("daemon offline"));
        };
        match client.send(&msg) {
            Ok(()) => Ok(()),
            Err(e) => {
                log::warn!("send failed, dropping link: {e}");
                self.dropped();
                Err(SinkError::from(format!("daemon send failed: {e}")))
            }
        }
    }
}
