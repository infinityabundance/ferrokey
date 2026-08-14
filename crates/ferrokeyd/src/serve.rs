//! The runtime broker (`ferrokeyd serve`) — the process that parses hostile
//! IPC (§3, §15, §41, §42).
//!
//! # Phase 3 contract
//!
//! * `serve` is **never root**: the supervisor pre-drops the identity before
//!   `exec`, and `serve` refuses to run as root unless the explicitly-named
//!   `--allow-root` development override is passed (§3, §7).
//! * The kernel device was created and verified by `init`; `serve` only
//!   *adopts* the transferred fd and verifies it again (§8, §10).
//! * The freeze order is mandatory (§41):
//!
//! ```text
//! adopt device fd ──▶ bind listener ──▶ FD inventory ──▶ capset(0)
//! ──▶ NO_NEW_PRIVS ──▶ seccomp ──▶ enforcement probes ──▶ serve clients
//! ```
//!
//! * The event loop is single-threaded `poll(2)` on the listener and client
//!   sockets; there is exactly **one** long-lived uinput device (§10) and at
//!   most `max_connections` sessions, each owning its held keys (§11, §12).
//! * Replies are queued per session with a hard cap and flushed when the
//!   socket is writable — a hostile client that stops reading cannot block
//!   the broker or grow memory unboundedly (§51: bounded pending output).
//! * On shutdown (SIGTERM/SIGINT or any fatal failure) every session's keys
//!   are released, the device is kept alive briefly so the key-ups reach the
//!   compositor, then the process exits and the kernel unregisters the
//!   device — no stuck keys survive (§22, §81).

use crate::device::{KeyDevice, RealDevice};
use crate::fds;
use crate::phase::{BrokerPhase, PhaseGuard};
use crate::sandbox;
use crate::security::{self, SecurityError};
use crate::session::{process_message, ClientSession, Outcome};
use crate::signals;
use crate::socket_path;
use ferrokey_protocol::{peer_identity, Decoder, Message};
use ferrokey_uinput::UinputDevice;
use nix::poll::{poll, PollFd, PollFlags, PollTimeout};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

/// The runtime parameters handed to `serve` by the supervisor.
#[derive(Debug, Clone)]
pub struct ServeArgs {
    /// The private socketpair end used to receive the device fd.
    pub handoff_fd: RawFd,
    pub socket_path: PathBuf,
    pub socket_mode: u32,
    pub allowed_uids: Vec<u32>,
    pub allowed_gids: Vec<u32>,
    /// Optional logind session-scope binding (§28, §99): clients must live
    /// in this session scope.
    pub session_scope: Option<String>,
    pub max_connections: usize,
    pub burst: u32,
    pub per_second: u32,
    pub max_held_keys: usize,
    pub device_name: String,
    /// The explicitly-named development override (§7); never set implicitly.
    pub allow_root: bool,
}

/// Errors from the runtime broker.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("cannot receive the device fd from init: {0}")]
    ReceiveFd(io::Error),
    #[error("cannot adopt/verify the device: {0}")]
    Adopt(#[from] ferrokey_uinput::UinputError),
    #[error("cannot bind the listener: {0}")]
    Bind(#[from] socket_path::SocketPathError),
    #[error("security freeze failed: {0}")]
    Security(#[from] SecurityError),
    #[error("cannot install seccomp: {0}")]
    Seccomp(io::Error),
    #[error("seccomp enforcement probe failed: {0}")]
    Probe(String),
    #[error("I/O error: {0}")]
    Io(String),
}

impl From<io::Error> for ServeError {
    fn from(e: io::Error) -> Self {
        ServeError::Io(e.to_string())
    }
}

/// Install SIGTERM/SIGINT handling.
pub fn install_signal_handlers() -> io::Result<()> {
    signals::install()
}

/// Hard cap on queued (not-yet-written) reply bytes per session (§51: a
/// client that stops reading must not make the broker buffer unboundedly;
/// the cap is far above any legitimate reply volume).
const MAX_PENDING_OUT: usize = 64 * 1024;

/// One connected client plus its decoding state and pending reply queue.
struct SessionSlot {
    stream: UnixStream,
    session: ClientSession,
    decoder: Decoder,
    /// Bounded reply bytes not yet flushed to the socket (§51).
    out: Vec<u8>,
    /// The connection must close once `out` is flushed.
    close_after_flush: bool,
    /// Why the connection is closing (recorded when the ERROR frame that
    /// triggered the close is queued; logged on removal, §105).
    close_reason: Option<String>,
}

impl SessionSlot {
    fn new(stream: UnixStream, session: ClientSession) -> Self {
        SessionSlot {
            stream,
            session,
            decoder: Decoder::new(),
            out: Vec::new(),
            close_after_flush: false,
            close_reason: None,
        }
    }

    /// Append a reply frame to the pending queue.
    ///
    /// # Postconditions
    /// * On `Err`, the queue cap would be exceeded — the client is not
    ///   draining; the connection must be dropped (§51).
    fn queue_reply(&mut self, msg: &Message) -> io::Result<()> {
        let frame = ferrokey_protocol::codec::encode(msg).map_err(io::Error::other)?;
        if self.out.len().saturating_add(frame.len()) > MAX_PENDING_OUT {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "pending reply buffer exceeded the hard cap",
            ));
        }
        self.out.extend_from_slice(&frame);
        Ok(())
    }

    /// Try to flush as much pending output as the socket accepts.
    ///
    /// # Postconditions
    /// * On `Ok(true)`, `out` is empty; on `Ok(false)` the socket is
    ///   temporarily not writable (EAGAIN) and more POLLOUT is awaited.
    fn flush_out(&mut self) -> io::Result<bool> {
        while !self.out.is_empty() {
            match self.stream.write(&self.out) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "write returned 0")),
                Ok(n) => {
                    self.out.drain(..n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(e) => return Err(e),
            }
        }
        Ok(true)
    }
}

/// Run the runtime broker to completion.
///
/// # Panics
/// Panics if the initial `PhaseGuard` is not in `Initializing` — an internal
/// invariant that is unit-tested (the guard is created immediately before
/// this assertion with no intervening transition).
pub fn run(args: ServeArgs) -> Result<(), ServeError> {
    // ── PHASE: Initializing ──────────────────────────────────────────────
    let mut phase = PhaseGuard::new();
    phase
        .expect(BrokerPhase::Initializing)
        .expect("fresh guard");

    let device_fd = fds::recv_owned_fd(args.handoff_fd).map_err(ServeError::ReceiveFd)?;
    // The handoff channel is no longer needed; close it now so the FD
    // inventory below sees only the intended set.
    nix::unistd::close(args.handoff_fd).ok();

    let device = UinputDevice::adopt(device_fd, &args.device_name, args.max_held_keys)?;
    let device_fd_num = device.raw_fd();
    let listener = socket_path::bind_secure(&args.socket_path, args.socket_mode)?;
    let listener_fd = listener.as_raw_fd();
    log::info!(
        "serve: device '{}' verified (fd {device_fd_num}); listener bound at {}",
        device.name(),
        args.socket_path.display()
    );

    // ── Session binding (§28, §99): the /proc dirfd for the peer cgroup
    //    lookup is opened BEFORE the freeze (the seccomp gate bakes the fd
    //    number into the runtime filter; the fd inventory accounts for it).
    let session_gate = match &args.session_scope {
        Some(scope) => {
            let gate = crate::session_scope::SessionScopeGate::open(scope)
                .map_err(|e| ServeError::Io(format!("cannot open the session-scope gate: {e}")))?;
            log::info!(
                "serve: bound to session scope '{}' (proc dirfd {})",
                gate.bound_scope(),
                gate.proc_dirfd()
            );
            Some(gate)
        }
        None => None,
    };

    // ── SECURITY FREEZE (§41) ────────────────────────────────────────────
    let mut extra_fds = vec![device_fd_num, listener_fd];
    if let Some(g) = &session_gate {
        extra_fds.push(g.proc_dirfd());
    }
    let expected_fds = security::expected_fds(&extra_fds);
    let report = security::verify_before_freeze(args.allow_root, &expected_fds)?;
    let seccomp_gate = session_gate.as_ref().map(|g| sandbox::SessionGate {
        proc_dirfd: g.proc_dirfd(),
        openat_flags: sandbox::SessionGate::READ_CGROUP_FLAGS,
    });
    sandbox::install_filter(seccomp_gate).map_err(ServeError::Seccomp)?;
    let probes =
        sandbox::prove_enforced(seccomp_gate).map_err(|e| ServeError::Probe(e.to_string()))?;
    if !probes.all_denied() {
        return Err(ServeError::Probe(format!(
            "forbidden syscalls were not all denied: {probes:?}"
        )));
    }
    log::info!("serve: sandbox frozen — {report}");
    log::info!("serve: enforcement probes — {probes}");

    phase
        .transition(BrokerPhase::Initializing, BrokerPhase::DeviceConfigured)
        .expect("freeze path");
    phase
        .transition(BrokerPhase::DeviceConfigured, BrokerPhase::Sandboxed)
        .expect("freeze path");
    phase
        .transition(BrokerPhase::Sandboxed, BrokerPhase::Serving)
        .expect("freeze path");

    // ── PHASE: Serving ───────────────────────────────────────────────────
    let mut runtime = Runtime {
        device: RealDevice(device),
        listener,
        sessions: Vec::new(),
        max_connections: args.max_connections,
        burst: args.burst,
        per_second: args.per_second,
        max_held_keys: args.max_held_keys,
        allowed_uids: args.allowed_uids.clone(),
        allowed_gids: args.allowed_gids.clone(),
        session_gate,
        phase,
    };
    log::info!("serve: accepting clients on {}", args.socket_path.display());
    runtime.serve_loop();

    // ── PHASE: ShuttingDown — release every session's keys, flush ────────
    let _ = runtime
        .phase
        .transition(BrokerPhase::Serving, BrokerPhase::ShuttingDown);
    let mut released = 0usize;
    let mut failures = 0usize;
    for slot in &mut runtime.sessions {
        let keys = slot.session.drain_held();
        let errors = runtime.device.release_keys(&keys);
        released += keys.len();
        failures += errors.len();
    }
    if failures > 0 {
        log::error!("serve: {failures} key(s) failed to release on shutdown");
    }
    // Keep the device alive briefly so the key-ups reach the compositor
    // before the fd closes and the kernel unregisters the device.
    log::info!("serve: released {released} key(s); flushing before exit");
    flush_delay(250);
    Ok(())
}

/// The broker's runtime: the device, the listener, the sessions.
struct Runtime {
    device: RealDevice,
    listener: std::os::unix::net::UnixListener,
    sessions: Vec<SessionSlot>,
    max_connections: usize,
    burst: u32,
    per_second: u32,
    max_held_keys: usize,
    allowed_uids: Vec<u32>,
    allowed_gids: Vec<u32>,
    /// Session-scope binding (§28, §99); `None` = UID/GID whitelist only.
    session_gate: Option<crate::session_scope::SessionScopeGate>,
    phase: PhaseGuard,
}

impl Runtime {
    fn serve_loop(&mut self) {
        loop {
            if signals::STOP.load(std::sync::atomic::Ordering::SeqCst) {
                log::info!("stop requested; shutting down");
                return;
            }
            // Build the poll set from raw fd numbers (no borrows of `self`):
            // the listener plus every session (POLLOUT when replies pend).
            let mut fds: Vec<PollFd> = Vec::with_capacity(1 + self.sessions.len());
            fds.push(PollFd::new(
                fds::borrow_fd(self.listener.as_raw_fd()),
                PollFlags::POLLIN,
            ));
            for slot in &self.sessions {
                let mut events = PollFlags::POLLIN;
                if !slot.out.is_empty() {
                    events |= PollFlags::POLLOUT;
                }
                fds.push(PollFd::new(fds::borrow_fd(slot.stream.as_raw_fd()), events));
            }
            match poll(&mut fds, PollTimeout::from(200u16)) {
                Ok(_) => {}
                Err(nix::errno::Errno::EINTR) => continue,
                Err(e) => {
                    log::error!("poll failed: {e}; shutting down");
                    return;
                }
            }

            // Accept new connections while the listener is readable.
            let listener_ready = fds[0].revents().is_some_and(|f| {
                f.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
            });
            if listener_ready {
                self.accept_pending();
            }

            // Service readable clients; collect the ones that must close.
            let mut to_close: Vec<(usize, String)> = Vec::new();
            for (i, fd) in fds.iter().enumerate().skip(1) {
                if fd.revents().is_none() {
                    continue;
                }
                if self.sessions[i - 1].close_after_flush {
                    // Shutting this session down; only its flush matters.
                    continue;
                }
                match self.service_client(i - 1) {
                    ServiceResult::Alive => {}
                    ServiceResult::Closed(reason) => to_close.push((i - 1, reason)),
                }
            }

            // Flush pending output on writable clients. A session waiting to
            // close (e.g. the rate-limit ERROR frame) is only removed once
            // its queue is empty — the peer must see the frame before the
            // FIN (a clean close, never an RST that drops it).
            for (i, fd) in fds.iter().enumerate().skip(1) {
                let writable = fd.revents().is_some_and(|f| {
                    f.intersects(PollFlags::POLLOUT | PollFlags::POLLHUP | PollFlags::POLLERR)
                });
                let slot = &mut self.sessions[i - 1];
                if slot.out.is_empty() && !slot.close_after_flush {
                    continue;
                }
                if !writable && !slot.close_after_flush {
                    continue;
                }
                match slot.flush_out() {
                    Ok(true) => {
                        if slot.close_after_flush {
                            let reason = slot
                                .close_reason
                                .take()
                                .unwrap_or_else(|| "close after error frame".into());
                            to_close.push((i - 1, reason));
                        }
                    }
                    Ok(false) => {} // wait for the next POLLOUT
                    Err(e) => {
                        log::info!("write failed: {e}");
                        to_close.push((i - 1, format!("write failed: {e}")));
                    }
                }
            }

            // Remove closed sessions, releasing exactly their keys (§12, §22).
            let mut seen: Vec<(usize, String)> = Vec::new();
            for entry in &to_close {
                if !seen.iter().any(|(idx, _)| *idx == entry.0) {
                    seen.push(entry.clone());
                }
            }
            for (idx, reason) in seen.iter().rev() {
                let mut slot = self.sessions.remove(*idx);
                let keys = slot.session.drain_held();
                let errors = self.device.release_keys(&keys);
                if !errors.is_empty() {
                    log::warn!("release on disconnect: {} key(s) failed", errors.len());
                }
                log::info!(
                    "peer {} disconnected: {reason}; {} key(s) released",
                    slot.session.peer,
                    keys.len()
                );
            }
        }
    }

    fn accept_pending(&mut self) {
        loop {
            if self.sessions.len() >= self.max_connections {
                // Reject excess connections (drop without reply).
                match fds::accept4_stream(self.listener.as_raw_fd()) {
                    Ok(stream) => drop(stream),
                    Err(_) => break,
                }
                continue;
            }
            match fds::accept4_stream(self.listener.as_raw_fd()) {
                Ok(stream) => match self.authorize(&stream) {
                    Ok(peer) => {
                        log::info!("peer {peer} authenticated");
                        self.sessions.push(SessionSlot::new(
                            stream,
                            ClientSession::new(
                                peer,
                                self.burst,
                                self.per_second,
                                self.max_held_keys,
                            ),
                        ));
                    }
                    Err(why) => {
                        log::info!("rejecting connection: {why}");
                        drop(stream);
                    }
                },
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    log::warn!("accept failed: {e}");
                    break;
                }
            }
        }
    }

    /// SO_PEERCRED authorization against the whitelist (§27), plus the
    /// optional session-scope binding (§28, §99): a client is accepted only
    /// when its UID/GID is whitelisted AND (if a session is bound) it lives
    /// in the bound logind session scope.
    fn authorize(&self, stream: &UnixStream) -> Result<ferrokey_protocol::PeerIdentity, String> {
        let peer = peer_identity(stream).map_err(|e| format!("SO_PEERCRED failed: {e}"))?;
        if !(self.allowed_uids.contains(&peer.uid) || self.allowed_gids.contains(&peer.gid)) {
            return Err(format!(
                "{peer} not in allowed_uids {:?} / allowed_gids {:?}",
                self.allowed_uids, self.allowed_gids
            ));
        }
        if let Some(gate) = &self.session_gate {
            if !gate.peer_is_in_bound_session(peer.pid) {
                return Err(format!(
                    "{peer} is not in the bound session scope '{}' — refusing (§99)",
                    gate.bound_scope()
                ));
            }
        }
        Ok(peer)
    }

    /// Read + process all available messages from one client.
    fn service_client(&mut self, index: usize) -> ServiceResult {
        let mut buf = [0u8; 8192];
        let n = match self.sessions[index].stream.read(&mut buf) {
            Ok(0) => return ServiceResult::Closed("peer closed (EOF)".into()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return ServiceResult::Alive,
            Err(e) => {
                log::info!("read error: {e}");
                return ServiceResult::Closed(format!("read error: {e}"));
            }
        };
        let messages = match self.sessions[index].decoder.push(&buf[..n]) {
            Ok(msgs) => msgs,
            Err(e) => {
                log::info!("protocol error: {e}");
                return ServiceResult::Closed(format!("protocol error: {e}"));
            }
        };
        for msg in messages {
            let outcome = process_message(&mut self.sessions[index].session, &mut self.device, msg);
            match outcome {
                Outcome::Keep => {}
                Outcome::Reply(reply) => {
                    if let Err(e) = self.sessions[index].queue_reply(&reply) {
                        log::info!("reply queue error: {e}");
                        return ServiceResult::Closed(format!("reply queue error: {e}"));
                    }
                }
                Outcome::ReplyAndClose(reply) => {
                    // The reply (an ERROR frame: rate limit, invalid state,
                    // handshake failure, …) is flushed to the peer before the
                    // connection closes; the flush loop removes the session
                    // once the queue is empty. The reason is recorded here so
                    // the disconnect log explains the drop (§105).
                    let reason = match &reply {
                        Message::Error(code, detail) => {
                            format!("error frame {code:?}: {detail}")
                        }
                        other => format!("close after reply {other:?}"),
                    };
                    if let Err(e) = self.sessions[index].queue_reply(&reply) {
                        log::info!("reply queue error: {e}");
                        return ServiceResult::Closed(format!("reply queue error: {e}"));
                    }
                    self.sessions[index].close_after_flush = true;
                    self.sessions[index].close_reason = Some(reason);
                    return ServiceResult::Alive;
                }
                Outcome::Close => {
                    // No reply; the pending output (if any) is flushed by the
                    // loop, then the session is removed.
                    return ServiceResult::Closed("handshake gate close".into());
                }
            }
        }
        ServiceResult::Alive
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServiceResult {
    Alive,
    Closed(String),
}

/// A short poll-based delay (no `nanosleep`: it is not in the seccomp
/// allowlist). Used to let key-up events flush to the compositor.
fn flush_delay(ms: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    let mut fds: [PollFd; 0] = [];
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let timeout_ms = u16::try_from(remaining.as_millis().min(65535))
            .unwrap_or(65535)
            .max(1);
        let _ = poll(&mut fds, PollTimeout::from(timeout_ms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flush_delay_returns() {
        let start = std::time::Instant::now();
        flush_delay(10);
        assert!(start.elapsed() >= std::time::Duration::from_millis(10));
    }

    #[test]
    fn reply_queue_cap_is_enforced() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let _ = b;
        let mut slot = SessionSlot::new(
            a,
            ClientSession::new(
                ferrokey_protocol::PeerIdentity {
                    uid: 1000,
                    gid: 1000,
                    pid: 1,
                },
                100,
                100,
                16,
            ),
        );
        // The cap is 64 KiB; each Ok frame is 7 bytes. Filling past the cap
        // must fail rather than grow unboundedly (§51).
        for _ in 0..(MAX_PENDING_OUT / 7) {
            slot.queue_reply(&Message::Ok).unwrap();
        }
        // One more frame (7 bytes) would exceed the cap.
        assert!(slot.queue_reply(&Message::Ok).is_err());
        assert!(slot.out.len() <= MAX_PENDING_OUT);
    }
}
