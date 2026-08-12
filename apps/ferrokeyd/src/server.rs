//! The daemon server: accept, authenticate, and serve protocol connections.
//!
//! Each connection owns its virtual keyboard device. When a connection dies,
//! the daemon first calls `release_all` and then drops the device — and even
//! if the daemon itself is SIGKILLed, the kernel releases every key when the
//! device is closed during process teardown. Stuck modifiers are not
//! survivable.

use crate::config::DaemonConfig;
use crate::device::{DeviceError, KeyboardDevice};
use crate::rate_limit::TokenBucket;
use ferrokey_protocol::{peer_identity, Decoder, ErrorCode, Message, PROTOCOL_VERSION};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Why a connection ended.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConnectionError {
    #[error("peer rejected: {0}")]
    Unauthorized(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("device error: {0}")]
    Device(String),
    #[error("rate limit exceeded")]
    RateLimited,
    #[error("I/O error: {0}")]
    Io(String),
    #[error("handshake incomplete: {0}")]
    Handshake(String),
    #[error("connection closed by peer")]
    Closed,
}

impl From<std::io::Error> for ConnectionError {
    fn from(e: std::io::Error) -> Self {
        ConnectionError::Io(e.to_string())
    }
}

/// Server-level errors.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("cannot bind socket {path}: {source}")]
    Bind {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot set socket permissions: {0}")]
    Permissions(String),
    #[error("I/O error: {0}")]
    Io(String),
}

impl From<std::io::Error> for ServerError {
    fn from(e: std::io::Error) -> Self {
        ServerError::Io(e.to_string())
    }
}

/// The running daemon.
pub struct Server {
    pub config: Arc<DaemonConfig>,
    listener: UnixListener,
    stop: Arc<AtomicBool>,
    live_connections: Arc<AtomicUsize>,
}

/// A connection plus its state, split out for testability.
pub(crate) struct ConnectionState {
    stream: UnixStream,
    decoder: Decoder,
    rate: TokenBucket,
    device: Box<dyn KeyboardDevice>,
    hello_received: bool,
    keyboard_created: bool,
}

impl Server {
    /// Bind the Unix socket (removing a stale one first).
    pub fn bind(config: DaemonConfig) -> Result<Self, ServerError> {
        let path = config.socket_path.clone();
        if path.exists() {
            // Remove a stale socket from a previous (possibly crashed) run.
            std::fs::remove_file(&path).map_err(|e| ServerError::Bind {
                path: path.display().to_string(),
                source: e,
            })?;
        }
        let listener = UnixListener::bind(&path).map_err(|source| ServerError::Bind {
            path: path.display().to_string(),
            source,
        })?;
        // Restrict the socket to the daemon's operator group.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(config.socket_mode))
            .map_err(|e| ServerError::Permissions(e.to_string()))?;
        log::info!("listening on {}", path.display());
        Ok(Server {
            config: Arc::new(config),
            listener,
            stop: Arc::new(AtomicBool::new(false)),
            live_connections: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Install SIGTERM/SIGINT handlers that request a graceful stop.
    ///
    /// Uses `SigSet::wait()` in a dedicated thread — entirely safe, no
    /// signal handler trampolines.
    pub fn install_signal_handlers(stop: &Arc<AtomicBool>) -> Result<(), ServerError> {
        use nix::sys::signal::{pthread_sigmask, SigSet, SigmaskHow, Signal};
        let mut set = SigSet::empty();
        set.add(Signal::SIGTERM);
        set.add(Signal::SIGINT);
        // Block the signals process-wide (inherited by spawned threads) so
        // they are delivered to the waiting thread below.
        pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&set), None)
            .map_err(|e| ServerError::Io(e.to_string()))?;
        let stop = stop.clone();
        std::thread::spawn(move || {
            if let Ok(sig) = set.wait() {
                log::info!("received signal {sig:?}; requesting stop");
                stop.store(true, Ordering::SeqCst);
            }
        });
        Ok(())
    }

    /// Run the accept loop until a stop signal arrives.
    pub fn run(&mut self) -> Result<(), ServerError> {
        self.listener
            .set_nonblocking(true)
            .map_err(|e| ServerError::Io(e.to_string()))?;
        let mut fds = [nix::poll::PollFd::new(
            std::os::fd::AsFd::as_fd(&self.listener),
            nix::poll::PollFlags::POLLIN,
        )];
        while !self.stop.load(Ordering::SeqCst) {
            match nix::poll::poll(&mut fds, nix::poll::PollTimeout::from(200u16)) {
                Ok(n) if n > 0 => {
                    while let Ok((stream, _)) = self.listener.accept() {
                        if self.live_connections.load(Ordering::SeqCst)
                            >= self.config.max_connections
                        {
                            log::warn!("rejecting connection: max_connections reached");
                            drop(stream);
                            continue;
                        }
                        self.live_connections.fetch_add(1, Ordering::SeqCst);
                        let config = self.config.clone();
                        let live = self.live_connections.clone();
                        std::thread::spawn(move || {
                            let result = serve_connection(stream, &config);
                            live.fetch_sub(1, Ordering::SeqCst);
                            match &result {
                                Ok(()) => log::info!("connection closed cleanly"),
                                Err(e) => log::info!("connection ended: {e}"),
                            }
                        });
                    }
                }
                Ok(_) => {} // timeout: re-check stop flag
                Err(e) => return Err(ServerError::Io(e.to_string())),
            }
        }
        log::info!("stop requested; shutting down");
        Ok(())
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Authorize a peer against the config whitelist.
fn authorize(
    peer: &ferrokey_protocol::PeerIdentity,
    config: &DaemonConfig,
) -> Result<(), ConnectionError> {
    if config.allowed_uids.contains(&peer.uid) || config.allowed_gids.contains(&peer.gid) {
        Ok(())
    } else {
        Err(ConnectionError::Unauthorized(format!(
            "{} not in allowed_uids {:?} / allowed_gids {:?}",
            peer, config.allowed_uids, config.allowed_gids
        )))
    }
}

/// Serve one connection to completion. On any failure the device is released
/// before returning.
pub fn serve_connection(stream: UnixStream, config: &DaemonConfig) -> Result<(), ConnectionError> {
    let peer = peer_identity(&stream).map_err(|e| ConnectionError::Io(e.to_string()))?;
    authorize(&peer, config)?;
    log::info!("peer {peer} authenticated");
    let mut state = ConnectionState {
        stream,
        decoder: Decoder::new(),
        rate: TokenBucket::new(config.rate.burst, config.rate.per_second),
        device: Box::new(crate::device::UinputKeyboard::new(
            &config.device_name,
            config.max_held_keys,
        )),
        hello_received: false,
        keyboard_created: false,
    };

    let result = connection_loop(&mut state);
    // Recovery contract: never leave stuck keys behind.
    log::info!("connection {peer} closing; releasing held keys");
    if let Err(e) = state.device.release_all() {
        log::error!("release_all failed on disconnect: {e}");
    }
    log::info!("connection {peer} release complete");
    // The release events must actually reach the compositor: destroying the
    // uinput device immediately after emitting them lets the kernel flush the
    // evdev client buffer before libinput reads it, silently dropping the
    // key-ups. Keep the device alive briefly so the events are delivered.
    std::thread::sleep(std::time::Duration::from_millis(250));
    result
}

fn connection_loop(state: &mut ConnectionState) -> Result<(), ConnectionError> {
    loop {
        let messages = read_messages(state)?;
        for message in messages {
            handle_message(state, message)?;
        }
        // If the peer has gone away (EOF), read_messages returns Closed.
    }
}

fn read_messages(state: &mut ConnectionState) -> Result<Vec<Message>, ConnectionError> {
    let mut buf = [0u8; 8192];
    loop {
        match std::io::Read::read(&mut state.stream, &mut buf) {
            Ok(0) => return Err(ConnectionError::Closed),
            Ok(n) => {
                let msgs = state
                    .decoder
                    .push(&buf[..n])
                    .map_err(|e| ConnectionError::Protocol(e.to_string()))?;
                if !msgs.is_empty() {
                    return Ok(msgs);
                }
                // A frame may span reads; loop for more.
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn send(state: &mut ConnectionState, msg: &Message) -> Result<(), ConnectionError> {
    let frame = ferrokey_protocol::codec::encode(msg)
        .map_err(|e| ConnectionError::Protocol(e.to_string()))?;
    state.stream.write_all(&frame)?;
    Ok(())
}

/// Read and discard whatever the peer sends for at most `budget`, so a
/// connection being torn down after a rate-limit violation ends with a clean
/// FIN instead of an RST (which would destroy the ERROR frame already sent).
/// Bounded: never blocks indefinitely, never decodes hostile input.
fn drain_peer(state: &mut ConnectionState, budget: std::time::Duration) {
    use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
    let deadline = std::time::Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut fd = [PollFd::new(
            std::os::fd::AsFd::as_fd(&state.stream),
            PollFlags::POLLIN,
        )];
        let timeout =
            PollTimeout::from(u16::try_from(remaining.as_millis().min(65535)).unwrap_or(65535));
        match poll(&mut fd, timeout) {
            Ok(n) if n > 0 => {
                let mut tmp = [0u8; 4096];
                match std::io::Read::read(&mut state.stream, &mut tmp) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            _ => break,
        }
    }
}

/// Process whatever messages are currently available without blocking.
/// Returns `Ok(true)` if any message was processed, `Ok(false)` if nothing
/// was pending, and `Err` on EOF or protocol failure. Used by tests to drive
/// the state machine deterministically.
#[cfg(test)]
pub(crate) fn pump(state: &mut ConnectionState) -> Result<bool, ConnectionError> {
    let mut buf = [0u8; 8192];
    let n = match std::io::Read::read(&mut state.stream, &mut buf) {
        Ok(0) => return Err(ConnectionError::Closed),
        Ok(n) => n,
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let messages = state
        .decoder
        .push(&buf[..n])
        .map_err(|e| ConnectionError::Protocol(e.to_string()))?;
    for message in messages {
        handle_message(state, message)?;
    }
    Ok(true)
}

fn handle_message(state: &mut ConnectionState, message: Message) -> Result<(), ConnectionError> {
    if !state.rate.allow() {
        let _ = send(
            state,
            &Message::Error(ErrorCode::RateLimited, "rate limit exceeded".into()),
        );
        // Drain the receive queue briefly before closing: dropping the socket
        // while unread bytes are queued makes the kernel send RST, which
        // discards the just-sent ERROR frame on the peer. A clean FIN lets the
        // client observe *why* it was dropped.
        drain_peer(state, std::time::Duration::from_millis(50));
        return Err(ConnectionError::RateLimited);
    }
    match message {
        Message::Hello { version, .. } => {
            if state.hello_received {
                let _ = send(
                    state,
                    &Message::Error(ErrorCode::Malformed, "duplicate HELLO".into()),
                );
                return Err(ConnectionError::Protocol("duplicate HELLO".into()));
            }
            state.hello_received = true;
            if version != PROTOCOL_VERSION {
                let _ = send(
                    state,
                    &Message::Error(
                        ErrorCode::VersionMismatch,
                        format!("expected version {PROTOCOL_VERSION}"),
                    ),
                );
                return Err(ConnectionError::Protocol(format!(
                    "version mismatch: got {version}, expected {PROTOCOL_VERSION}"
                )));
            }
            Ok(())
        }
        Message::CreateKeyboard => {
            if !state.hello_received {
                let _ = send(
                    state,
                    &Message::Error(
                        ErrorCode::Malformed,
                        "HELLO must precede CREATE_KEYBOARD".into(),
                    ),
                );
                return Err(ConnectionError::Handshake(
                    "CREATE_KEYBOARD before HELLO".into(),
                ));
            }
            if state.keyboard_created {
                let _ = send(
                    state,
                    &Message::Error(ErrorCode::AlreadyCreated, "keyboard already created".into()),
                );
                return Err(ConnectionError::Protocol(
                    "duplicate CREATE_KEYBOARD".into(),
                ));
            }
            state.keyboard_created = true;
            state.device.create().map_err(|e| {
                let _ = send(
                    state,
                    &Message::Error(ErrorCode::DeviceError, e.to_string()),
                );
                ConnectionError::Device(e.to_string())
            })?;
            log::info!("virtual keyboard created");
            send(state, &Message::Ok)
        }
        Message::KeyDown(code) => {
            if !state.keyboard_created {
                let _ = send(
                    state,
                    &Message::Error(
                        ErrorCode::Malformed,
                        "KEY_DOWN before CREATE_KEYBOARD".into(),
                    ),
                );
                return Err(ConnectionError::Handshake(
                    "KEY_DOWN before CREATE_KEYBOARD".into(),
                ));
            }
            if !state.device.capability_codes().contains(&u32::from(code)) {
                let _ = send(
                    state,
                    &Message::Error(
                        ErrorCode::UnknownKey,
                        format!("key {code} not in capability set"),
                    ),
                );
                return Err(ConnectionError::Protocol(format!(
                    "key {code} outside capability set"
                )));
            }
            log::info!("key_down {code}");
            state.device.key_down(code).map_err(|e| {
                let _ = send(state, &Message::Error(map_device_error(&e), e.to_string()));
                ConnectionError::Device(e.to_string())
            })
        }
        Message::KeyUp(code) => {
            if !state.keyboard_created {
                let _ = send(
                    state,
                    &Message::Error(ErrorCode::Malformed, "KEY_UP before CREATE_KEYBOARD".into()),
                );
                return Err(ConnectionError::Handshake(
                    "KEY_UP before CREATE_KEYBOARD".into(),
                ));
            }
            log::info!("key_up {code}");
            state.device.key_up(code).map_err(|e| {
                let _ = send(state, &Message::Error(map_device_error(&e), e.to_string()));
                ConnectionError::Device(e.to_string())
            })
        }
        Message::KeyRepeat(code) => {
            if !state.keyboard_created {
                let _ = send(
                    state,
                    &Message::Error(
                        ErrorCode::Malformed,
                        "KEY_REPEAT before CREATE_KEYBOARD".into(),
                    ),
                );
                return Err(ConnectionError::Handshake(
                    "KEY_REPEAT before CREATE_KEYBOARD".into(),
                ));
            }
            if !state.device.capability_codes().contains(&u32::from(code)) {
                let _ = send(
                    state,
                    &Message::Error(
                        ErrorCode::UnknownKey,
                        format!("key {code} not in capability set"),
                    ),
                );
                return Err(ConnectionError::Protocol(format!(
                    "key {code} outside capability set"
                )));
            }
            log::info!("key_repeat {code}");
            state.device.key_repeat(code).map_err(|e| {
                let _ = send(state, &Message::Error(map_device_error(&e), e.to_string()));
                ConnectionError::Device(e.to_string())
            })
        }
        Message::ReleaseAll => {
            state
                .device
                .release_all()
                .map_err(|e| ConnectionError::Device(e.to_string()))?;
            send(state, &Message::Ok)
        }
        Message::Ping(nonce) => send(state, &Message::Pong(nonce)),
        Message::Pong(_) => Err(ConnectionError::Protocol(
            "unexpected PONG from client".into(),
        )),
        Message::Ok => Err(ConnectionError::Protocol(
            "unexpected OK from client".into(),
        )),
        Message::Error(code, msg) => {
            let _ = send(
                state,
                &Message::Error(ErrorCode::Malformed, "client sent ERROR".into()),
            );
            Err(ConnectionError::Protocol(format!(
                "client sent ERROR({code:?}): {msg}"
            )))
        }
    }
}

fn map_device_error(e: &DeviceError) -> ErrorCode {
    match e {
        DeviceError::UnknownKey(_) => ErrorCode::UnknownKey,
        DeviceError::KeyUpWithoutDown(_) | DeviceError::Rollover(_) => ErrorCode::InvalidKeyState,
        DeviceError::Create(_) => ErrorCode::DeviceError,
        DeviceError::Io(_) => ErrorCode::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MockKeyboard;
    use ferrokey_protocol::codec::encode;
    use std::io::Read as _;

    struct Harness {
        client: UnixStream,
        state: ConnectionState,
    }

    impl Harness {
        fn new() -> Self {
            let (client, server) = UnixStream::pair().unwrap();
            client.set_nonblocking(true).unwrap();
            server.set_nonblocking(true).unwrap();
            let state = ConnectionState {
                stream: server,
                decoder: Decoder::new(),
                rate: TokenBucket::new(1000, 1000),
                device: Box::new(MockKeyboard::new()),
                hello_received: false,
                keyboard_created: false,
            };
            Harness { client, state }
        }

        fn send(&mut self, msg: &Message) {
            self.client.write_all(&encode(msg).unwrap()).unwrap();
        }

        /// Read a server reply, if any is pending.
        fn reply(&mut self) -> Option<Message> {
            let mut buf = [0u8; 4096];
            let n = match self.client.read(&mut buf) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return None,
                Err(_) => return None,
            };
            if n == 0 {
                return None;
            }
            let mut dec = Decoder::new();
            dec.push(&buf[..n]).ok().and_then(|mut v| {
                if v.is_empty() {
                    None
                } else {
                    Some(v.remove(0))
                }
            })
        }

        fn drain_replies(&mut self) -> Vec<Message> {
            let mut out = Vec::new();
            while let Some(m) = self.reply() {
                out.push(m);
            }
            out
        }

        fn handshake(&mut self) {
            self.send(&Message::Hello {
                version: 1,
                client_name: "test-ui".into(),
            });
            self.send(&Message::CreateKeyboard);
            self.pump().expect("handshake must succeed");
        }

        fn pump(&mut self) -> Result<bool, ConnectionError> {
            pump(&mut self.state)
        }
    }

    #[test]
    fn full_handshake_and_key_flow() {
        let mut h = Harness::new();
        h.handshake();
        let replies = h.drain_replies();
        assert_eq!(replies, vec![Message::Ok]);

        h.send(&Message::KeyDown(30));
        h.send(&Message::KeyUp(30));
        h.pump().unwrap();

        let device: &MockKeyboard = h
            .state
            .device
            .as_any()
            .downcast_ref::<MockKeyboard>()
            .unwrap();
        assert_eq!(device.events, vec![(true, 30), (false, 30)]);
    }

    #[test]
    fn key_up_without_down_is_rejected() {
        let mut h = Harness::new();
        h.handshake();
        h.drain_replies();

        h.send(&Message::KeyUp(30));
        let err = h.pump().unwrap_err();
        assert!(matches!(err, ConnectionError::Device(_)));
        let replies = h.drain_replies();
        assert!(matches!(
            replies[0],
            Message::Error(ErrorCode::InvalidKeyState, _)
        ));
    }

    #[test]
    fn unknown_key_code_is_rejected() {
        let mut h = Harness::new();
        h.handshake();
        h.drain_replies();

        h.send(&Message::KeyDown(0xFFFF));
        // The capability check rejects the key before it ever reaches the
        // device: a protocol violation, not a device error.
        let err = h.pump().unwrap_err();
        assert!(matches!(err, ConnectionError::Protocol(_)));
        let replies = h.drain_replies();
        assert!(matches!(
            replies[0],
            Message::Error(ErrorCode::UnknownKey, _)
        ));
    }

    #[test]
    fn key_repeat_of_held_key_emits_repeat() {
        let mut h = Harness::new();
        h.handshake();
        h.drain_replies();

        // Repeat is not a state transition: it must be routed to the device
        // and pass through for a held key without touching the ledger.
        h.send(&Message::KeyDown(30));
        h.send(&Message::KeyRepeat(30));
        h.pump().unwrap();

        let device: &MockKeyboard = h
            .state
            .device
            .as_any()
            .downcast_ref::<MockKeyboard>()
            .unwrap();
        assert_eq!(device.events, vec![(true, 30), (true, 30)]);
    }

    #[test]
    fn key_repeat_without_held_key_is_rejected() {
        let mut h = Harness::new();
        h.handshake();
        h.drain_replies();

        // Autorepeating a key that is not held is invalid state (the mock
        // enforces it; uinput's ledger does the same on the device side).
        h.send(&Message::KeyRepeat(30));
        let err = h.pump().unwrap_err();
        assert!(matches!(err, ConnectionError::Device(_)));
        let replies = h.drain_replies();
        assert!(matches!(
            replies[0],
            Message::Error(ErrorCode::InvalidKeyState, _)
        ));
    }

    #[test]
    fn key_repeat_before_create_is_rejected() {
        let mut h = Harness::new();
        h.send(&Message::Hello {
            version: 1,
            client_name: "x".into(),
        });
        h.send(&Message::KeyRepeat(30));
        let err = h.pump().unwrap_err();
        assert!(matches!(err, ConnectionError::Handshake(_)));
        let replies = h.drain_replies();
        assert!(matches!(
            replies[0],
            Message::Error(ErrorCode::Malformed, _)
        ));
    }

    #[test]
    fn key_repeat_unknown_code_is_rejected() {
        let mut h = Harness::new();
        h.handshake();
        h.drain_replies();

        h.send(&Message::KeyRepeat(0xFFFF));
        let err = h.pump().unwrap_err();
        assert!(matches!(err, ConnectionError::Protocol(_)));
        let replies = h.drain_replies();
        assert!(matches!(
            replies[0],
            Message::Error(ErrorCode::UnknownKey, _)
        ));
    }

    #[test]
    fn version_mismatch_is_rejected() {
        let mut h = Harness::new();
        h.send(&Message::Hello {
            version: 99,
            client_name: "bad".into(),
        });
        let err = h.pump().unwrap_err();
        assert!(matches!(err, ConnectionError::Protocol(_)));
        let replies = h.drain_replies();
        assert!(matches!(
            replies[0],
            Message::Error(ErrorCode::VersionMismatch, _)
        ));
    }

    #[test]
    fn create_before_hello_is_rejected() {
        let mut h = Harness::new();
        h.send(&Message::CreateKeyboard);
        let err = h.pump().unwrap_err();
        assert!(matches!(err, ConnectionError::Handshake(_)));
        let replies = h.drain_replies();
        assert!(matches!(
            replies[0],
            Message::Error(ErrorCode::Malformed, _)
        ));
    }

    #[test]
    fn key_before_create_is_rejected() {
        let mut h = Harness::new();
        h.send(&Message::Hello {
            version: 1,
            client_name: "x".into(),
        });
        h.send(&Message::KeyDown(30));
        let err = h.pump().unwrap_err();
        assert!(matches!(err, ConnectionError::Handshake(_)));
        let replies = h.drain_replies();
        assert!(matches!(
            replies[0],
            Message::Error(ErrorCode::Malformed, _)
        ));
    }

    #[test]
    fn release_all_returns_ok_and_releases() {
        let mut h = Harness::new();
        h.handshake();
        h.drain_replies();
        h.send(&Message::KeyDown(42));
        h.pump().unwrap();
        h.send(&Message::ReleaseAll);
        h.pump().unwrap();
        let replies = h.drain_replies();
        assert_eq!(replies, vec![Message::Ok]);
        let device: &MockKeyboard = h
            .state
            .device
            .as_any()
            .downcast_ref::<MockKeyboard>()
            .unwrap();
        assert!(device.events.is_empty());
        assert_eq!(device.released_all, 1);
    }

    #[test]
    fn ping_gets_pong() {
        let mut h = Harness::new();
        h.handshake();
        h.drain_replies();
        h.send(&Message::Ping(0xDEAD_BEEF));
        h.pump().unwrap();
        assert_eq!(h.drain_replies(), vec![Message::Pong(0xDEAD_BEEF)]);
    }

    #[test]
    fn malformed_frames_are_fatal() {
        let mut h = Harness::new();
        h.handshake();
        h.drain_replies();
        // Garbage bytes: bad magic.
        h.client.write_all(b"JUNKJUNK").unwrap();
        let err = h.pump().unwrap_err();
        assert!(matches!(err, ConnectionError::Protocol(_)));
    }

    #[test]
    fn rate_limit_rejects_flood() {
        let mut h = Harness::new();
        // Tiny rate limit: burst of 2, so the handshake exhausts the bucket.
        h.state.rate = TokenBucket::new(2, 1);
        h.send(&Message::Hello {
            version: 1,
            client_name: "x".into(),
        });
        h.send(&Message::CreateKeyboard);
        h.pump().unwrap();
        h.drain_replies();

        h.send(&Message::Ping(1));
        let err = h.pump().unwrap_err();
        assert!(matches!(err, ConnectionError::RateLimited));
        let replies = h.drain_replies();
        assert!(replies
            .iter()
            .any(|m| matches!(m, Message::Error(ErrorCode::RateLimited, _))));
    }
}
