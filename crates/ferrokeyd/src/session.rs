//! Per-client session state and the pure protocol-processing core.
//!
//! # Phase 3 design
//!
//! * A session is **logical client state only** (§9): `OPEN_SESSION` never
//!   creates, configures or destroys a kernel device. The broker owns exactly
//!   one pre-created keyboard (§10).
//! * `process_message` transforms hostile protocol bytes (already decoded by
//!   the framing layer) into a closed set of outcomes against the
//!   [`KeyDevice`] — the kernel-facing layer only ever sees validated key
//!   events (§18, §20).
//! * Key ownership is per-session (§12): every depressed key belongs to the
//!   session that pressed it; on disconnect, exactly that session's keys are
//!   released. A second session pressing an already-held key is rejected.
//! * The session's held-key set plus the device ledger are the authoritative
//!   record (§22); the kernel is never trusted to track Ferrokey's state.
//! * Rate limiting happens **before** any expensive work (§77): a message
//!   that fails the token bucket never reaches parsing/validation/device.

use crate::device::{DeviceError, KeyDevice};
use crate::rate_limit::TokenBucket;
use ferrokey_core::PhysicalKey;
use ferrokey_protocol::{ErrorCode, Message, PROTOCOL_VERSION};
use std::collections::BTreeSet;

/// What the connection loop should do after processing one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Write a reply and keep the connection open.
    Reply(Message),
    /// Write a reply, then tear the connection down.
    ReplyAndClose(Message),
    /// Tear the connection down without a reply.
    Close,
    /// Nothing to write; keep the connection open.
    Keep,
}

/// The state of one connected client session.
#[derive(Debug)]
pub struct ClientSession {
    /// The kernel-reported peer identity (SO_PEERCRED) — never client input.
    pub peer: ferrokey_protocol::PeerIdentity,
    hello_received: bool,
    session_open: bool,
    /// Keys pressed by *this* session (ownership record, §12).
    held: BTreeSet<u16>,
    rate: TokenBucket,
    /// Maximum keys this session may hold (from the bounded config §24).
    max_held_keys: usize,
}

impl ClientSession {
    pub fn new(
        peer: ferrokey_protocol::PeerIdentity,
        burst: u32,
        per_second: u32,
        max_held_keys: usize,
    ) -> Self {
        ClientSession {
            peer,
            hello_received: false,
            session_open: false,
            held: BTreeSet::new(),
            rate: TokenBucket::new(burst, per_second),
            max_held_keys,
        }
    }

    /// The keys currently held by this session.
    pub fn held_keys(&self) -> &BTreeSet<u16> {
        &self.held
    }

    pub fn has_open_session(&self) -> bool {
        self.session_open
    }

    /// Take this session's keys (used on disconnect: release exactly these).
    pub fn drain_held(&mut self) -> Vec<u16> {
        let keys: Vec<u16> = self.held.iter().copied().collect();
        self.held.clear();
        keys
    }
}

/// Process one decoded protocol message against the session and the device.
///
/// # Preconditions
/// * `msg` has already passed the framing decoder (magic/length/opcode
///   validation). This function performs the *semantic* validation: handshake
///   order, capability membership, key ownership, state transitions.
/// * The rate limit is enforced here, before validation work (§77).
///
/// # Postconditions
/// * On `ReplyAndClose`, the connection must be torn down by the caller after
///   the reply is written; on `Close` the connection is torn down silently.
/// * On error outcomes the session state is left unchanged (no partial
///   mutation): hostile input cannot corrupt another session's state (§12).
pub fn process_message(
    session: &mut ClientSession,
    device: &mut dyn KeyDevice,
    msg: Message,
) -> Outcome {
    // Rate limit first: hostile floods never reach validation (§77).
    if !session.rate.allow() {
        return Outcome::ReplyAndClose(Message::Error(
            ErrorCode::RateLimited,
            "rate limit exceeded".into(),
        ));
    }

    match msg {
        Message::Hello { version, .. } => {
            if session.hello_received {
                return err_close(ErrorCode::Malformed, "duplicate HELLO");
            }
            session.hello_received = true;
            if version != PROTOCOL_VERSION {
                return err_close(
                    ErrorCode::VersionMismatch,
                    format!("expected version {PROTOCOL_VERSION}"),
                );
            }
            Outcome::Keep
        }
        Message::OpenSession => {
            if !session.hello_received {
                return err_close(ErrorCode::Handshake, "HELLO must precede OPEN_SESSION");
            }
            if session.session_open {
                return err_close(ErrorCode::AlreadyCreated, "session already open");
            }
            session.session_open = true;
            Outcome::Reply(Message::Ok)
        }
        Message::KeyDown(code) => {
            if !gate(session, device) {
                return Outcome::Close;
            }
            // Protocol-boundary validation (§20): capability membership.
            if !device.is_capable(code) {
                return err_close(
                    ErrorCode::UnknownKey,
                    format!("key {code} not in capability set"),
                );
            }
            let Some(key) = PhysicalKey::from_linux_code(u32::from(code)) else {
                // Belt-and-braces: capability implies a known key, but a
                // mapping gap must never reach the device layer.
                return err_close(ErrorCode::UnknownKey, format!("key {code} unmapped"));
            };
            // Ownership validation (§12, §22): duplicate down rejected.
            if session.held.contains(&code) {
                return err_close(
                    ErrorCode::InvalidKeyState,
                    format!("duplicate KEY_DOWN for {code}"),
                );
            }
            if session.held.len() >= session.max_held_keys {
                return err_close(
                    ErrorCode::InvalidKeyState,
                    format!("session held-key limit ({}) reached", session.max_held_keys),
                );
            }
            // Device-boundary validation: capability + global ledger (§20).
            match device.key_down(key) {
                Ok(()) => {
                    session.held.insert(code);
                    Outcome::Keep
                }
                Err(e) => device_error_outcome(code, &e),
            }
        }
        Message::KeyUp(code) => {
            if !gate(session, device) {
                return Outcome::Close;
            }
            if !device.is_capable(code) {
                return err_close(
                    ErrorCode::UnknownKey,
                    format!("key {code} not in capability set"),
                );
            }
            let Some(key) = PhysicalKey::from_linux_code(u32::from(code)) else {
                return err_close(ErrorCode::UnknownKey, format!("key {code} unmapped"));
            };
            if !session.held.contains(&code) {
                return err_close(
                    ErrorCode::InvalidKeyState,
                    format!("KEY_UP for {code} without a matching KEY_DOWN"),
                );
            }
            match device.key_up(key) {
                Ok(()) => {
                    session.held.remove(&code);
                    Outcome::Keep
                }
                Err(e) => device_error_outcome(code, &e),
            }
        }
        Message::KeyRepeat(code) => {
            if !gate(session, device) {
                return Outcome::Close;
            }
            if !device.is_capable(code) {
                return err_close(
                    ErrorCode::UnknownKey,
                    format!("key {code} not in capability set"),
                );
            }
            let Some(key) = PhysicalKey::from_linux_code(u32::from(code)) else {
                return err_close(ErrorCode::UnknownKey, format!("key {code} unmapped"));
            };
            // §22: repeat-without-down is rejected.
            if !session.held.contains(&code) {
                return err_close(
                    ErrorCode::InvalidKeyState,
                    format!("KEY_REPEAT for {code} without a held key"),
                );
            }
            match device.key_repeat(key) {
                Ok(()) => Outcome::Keep,
                Err(e) => device_error_outcome(code, &e),
            }
        }
        Message::ReleaseAll => {
            if !gate(session, device) {
                return Outcome::Close;
            }
            let keys = session.drain_held();
            // §23: fail-safe — the device layer attempts every key.
            let errors = device.release_keys(&keys);
            if errors.is_empty() {
                Outcome::Reply(Message::Ok)
            } else {
                Outcome::Reply(Message::Error(
                    ErrorCode::Internal,
                    format!("partial release failure: {} key(s)", errors.len()),
                ))
            }
        }
        Message::Ping(nonce) => Outcome::Reply(Message::Pong(nonce)),
        Message::Pong(_) => err_close(ErrorCode::Malformed, "unexpected PONG from client"),
        Message::Ok => err_close(ErrorCode::Malformed, "unexpected OK from client"),
        Message::Error(code, msg) => err_close(
            ErrorCode::Malformed,
            format!("client sent ERROR({code:?}): {msg}"),
        ),
    }
}

/// The handshake gate: HELLO + OPEN_SESSION must precede key operations.
/// A session that never opened is torn down without a reply.
fn gate(session: &ClientSession, _device: &dyn KeyDevice) -> bool {
    session.session_open
}

fn err_close(code: ErrorCode, detail: impl Into<String>) -> Outcome {
    Outcome::ReplyAndClose(Message::Error(code, detail.into()))
}

fn device_error_outcome(code: u16, e: &DeviceError) -> Outcome {
    let detail = e.to_string();
    let error_code = match e {
        DeviceError::UnknownKey(_) => ErrorCode::UnknownKey,
        DeviceError::KeyUpWithoutDown(_) | DeviceError::Rollover(_) | DeviceError::KeyBusy(_) => {
            ErrorCode::InvalidKeyState
        }
        DeviceError::Io(_) => ErrorCode::Internal,
    };
    let _ = code;
    err_close(error_code, detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::MockKeyDevice;

    fn session() -> ClientSession {
        ClientSession::new(
            ferrokey_protocol::PeerIdentity {
                uid: 1000,
                gid: 1000,
                pid: 42,
            },
            10_000,
            10_000,
            16,
        )
    }

    fn handshake(s: &mut ClientSession, d: &mut dyn KeyDevice) {
        assert_eq!(
            process_message(
                s,
                d,
                Message::Hello {
                    version: PROTOCOL_VERSION,
                    client_name: "test".into()
                }
            ),
            Outcome::Keep
        );
        assert_eq!(
            process_message(s, d, Message::OpenSession),
            Outcome::Reply(Message::Ok)
        );
    }

    fn code(key: PhysicalKey) -> u16 {
        u16::try_from(key.linux_code()).unwrap()
    }

    #[test]
    fn full_session_flow() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        handshake(&mut s, &mut d);
        assert_eq!(
            process_message(&mut s, &mut d, Message::KeyDown(code(PhysicalKey::A))),
            Outcome::Keep
        );
        assert!(s.held_keys().contains(&code(PhysicalKey::A)));
        assert_eq!(
            process_message(&mut s, &mut d, Message::KeyUp(code(PhysicalKey::A))),
            Outcome::Keep
        );
        assert!(s.held_keys().is_empty());
        assert_eq!(
            process_message(&mut s, &mut d, Message::ReleaseAll),
            Outcome::Reply(Message::Ok)
        );
    }

    #[test]
    fn key_ops_require_open_session() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        s.hello_received = true; // hello ok, but session not opened
        let out = process_message(&mut s, &mut d, Message::KeyDown(code(PhysicalKey::A)));
        assert!(matches!(out, Outcome::Close));
    }

    #[test]
    fn duplicate_down_is_rejected() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        handshake(&mut s, &mut d);
        process_message(&mut s, &mut d, Message::KeyDown(code(PhysicalKey::A)));
        let out = process_message(&mut s, &mut d, Message::KeyDown(code(PhysicalKey::A)));
        assert!(matches!(
            out,
            Outcome::ReplyAndClose(Message::Error(ErrorCode::InvalidKeyState, _))
        ));
        // State unchanged: the session still holds the key exactly once.
        assert_eq!(s.held_keys().len(), 1);
    }

    #[test]
    fn up_without_down_is_rejected() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        handshake(&mut s, &mut d);
        let out = process_message(&mut s, &mut d, Message::KeyUp(code(PhysicalKey::A)));
        assert!(matches!(
            out,
            Outcome::ReplyAndClose(Message::Error(ErrorCode::InvalidKeyState, _))
        ));
    }

    #[test]
    fn repeat_without_down_is_rejected() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        handshake(&mut s, &mut d);
        let out = process_message(&mut s, &mut d, Message::KeyRepeat(code(PhysicalKey::A)));
        assert!(matches!(
            out,
            Outcome::ReplyAndClose(Message::Error(ErrorCode::InvalidKeyState, _))
        ));
    }

    #[test]
    fn unknown_code_is_rejected_at_protocol_boundary() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        handshake(&mut s, &mut d);
        for bad in [0u16, 0x2ff, u16::MAX, 0x100] {
            let out = process_message(&mut s, &mut d, Message::KeyDown(bad));
            assert!(
                matches!(
                    out,
                    Outcome::ReplyAndClose(Message::Error(ErrorCode::UnknownKey, _))
                ),
                "code {bad} must be rejected"
            );
        }
        // The kernel-facing device saw nothing.
        assert!(d.events.is_empty());
    }

    #[test]
    fn release_all_empties_session_and_device() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        handshake(&mut s, &mut d);
        process_message(&mut s, &mut d, Message::KeyDown(code(PhysicalKey::A)));
        process_message(&mut s, &mut d, Message::KeyDown(code(PhysicalKey::B)));
        assert_eq!(s.held_keys().len(), 2);
        let out = process_message(&mut s, &mut d, Message::ReleaseAll);
        assert_eq!(out, Outcome::Reply(Message::Ok));
        assert!(s.held_keys().is_empty());
        assert!(!d.is_held(code(PhysicalKey::A)) && !d.is_held(code(PhysicalKey::B)));
    }

    #[test]
    fn disconnect_releases_exactly_this_sessions_keys() {
        // Two sessions share one device; each holds a different key. The
        // first session's disconnect must release only its own key (§12).
        let mut s1 = session();
        let mut s2 = session();
        let mut d = MockKeyDevice::new();
        handshake(&mut s1, &mut d);
        handshake(&mut s2, &mut d);
        process_message(&mut s1, &mut d, Message::KeyDown(code(PhysicalKey::A)));
        process_message(&mut s2, &mut d, Message::KeyDown(code(PhysicalKey::B)));
        let s1_keys = s1.drain_held();
        let errors = d.release_keys(&s1_keys);
        assert!(errors.is_empty());
        assert!(!d.is_held(code(PhysicalKey::A)));
        assert!(
            d.is_held(code(PhysicalKey::B)),
            "other session's key untouched"
        );
        // The other session can now release its own key normally.
        let s2_keys = s2.drain_held();
        assert_eq!(s2_keys, vec![code(PhysicalKey::B)]);
        assert!(d.release_keys(&s2_keys).is_empty());
        assert!(!d.is_held(code(PhysicalKey::B)));
    }

    #[test]
    fn cross_session_key_busy_is_rejected() {
        // Session 1 holds A; session 2 pressing A must be rejected by the
        // device-boundary check (global ledger) — §12 ownership.
        let mut s1 = session();
        let mut s2 = session();
        let mut d = MockKeyDevice::new();
        handshake(&mut s1, &mut d);
        handshake(&mut s2, &mut d);
        process_message(&mut s1, &mut d, Message::KeyDown(code(PhysicalKey::A)));
        let out = process_message(&mut s2, &mut d, Message::KeyDown(code(PhysicalKey::A)));
        assert!(matches!(
            out,
            Outcome::ReplyAndClose(Message::Error(ErrorCode::InvalidKeyState, _))
        ));
        // Session 2 must not have recorded ownership of A.
        assert!(!s2.held_keys().contains(&code(PhysicalKey::A)));
    }

    #[test]
    fn ping_gets_pong() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        assert_eq!(
            process_message(&mut s, &mut d, Message::Ping(7)),
            Outcome::Reply(Message::Pong(7))
        );
    }

    #[test]
    fn server_only_messages_from_client_are_rejected() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        for msg in [
            Message::Ok,
            Message::Pong(1),
            Message::Error(ErrorCode::Malformed, "x".into()),
        ] {
            assert!(matches!(
                process_message(&mut s, &mut d, msg),
                Outcome::ReplyAndClose(_)
            ));
        }
    }

    #[test]
    fn version_mismatch_is_rejected() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        let out = process_message(
            &mut s,
            &mut d,
            Message::Hello {
                version: PROTOCOL_VERSION + 1,
                client_name: "x".into(),
            },
        );
        assert!(matches!(
            out,
            Outcome::ReplyAndClose(Message::Error(ErrorCode::VersionMismatch, _))
        ));
    }

    #[test]
    fn rate_limit_precedes_validation() {
        // A session with a spent bucket must be dropped before any key work.
        let mut s = session();
        s.rate = TokenBucket::new(1, 1);
        s.rate.allow(); // exhaust the single token
        let mut d = MockKeyDevice::new();
        // Even an invalid message is dropped for rate, not parsed further.
        let out = process_message(&mut s, &mut d, Message::Ping(1));
        assert!(matches!(
            out,
            Outcome::ReplyAndClose(Message::Error(ErrorCode::RateLimited, _))
        ));
        assert!(d.events.is_empty());
    }

    #[test]
    fn duplicate_open_session_is_rejected() {
        let mut s = session();
        let mut d = MockKeyDevice::new();
        process_message(
            &mut s,
            &mut d,
            Message::Hello {
                version: PROTOCOL_VERSION,
                client_name: "x".into(),
            },
        );
        assert_eq!(
            process_message(&mut s, &mut d, Message::OpenSession),
            Outcome::Reply(Message::Ok)
        );
        let out = process_message(&mut s, &mut d, Message::OpenSession);
        assert!(matches!(
            out,
            Outcome::ReplyAndClose(Message::Error(ErrorCode::AlreadyCreated, _))
        ));
    }

    // -----------------------------------------------------------------------
    // M9 (§54, §87, §88): model-based property tests over the state machine.
    // Deterministic seeded PRNG (xorshift64*) — no external property-test
    // dependency (the broker minimizes its dependency tree, §83).
    // -----------------------------------------------------------------------

    /// xorshift64* — deterministic, allocation-free.
    fn next_rand(rng: &mut u64) -> u64 {
        *rng ^= *rng << 13;
        *rng ^= *rng >> 7;
        *rng ^= *rng << 17;
        (*rng).wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// One generated operation; the model applies it to the reference state
    /// and the implementation is driven with the same op, then compared.
    #[derive(Debug, Clone, Copy)]
    enum Op {
        Hello,
        OpenSession,
        KeyDown(u16),
        KeyUp(u16),
        KeyRepeat(u16),
        ReleaseAll,
        Ping(u64),
        InvalidDown, // deliberately out-of-capability code
        Disconnect,  // session teardown: drain + release exactly these keys
    }

    /// The reference model: the session state as the protocol defines it.
    #[derive(Debug, Default)]
    struct Model {
        hello: bool,
        open: bool,
        held: std::collections::BTreeSet<u16>,
    }

    /// Apply `op` to the *logical* model (no device calls: the model is a
    /// pure reference and the implementation drives the shared device — see
    /// [`drive`]). Mirrors the documented state machine (§12, §18, §22).
    /// Returns whether the session survived (Close resets). When the session
    /// dies, the model mirrors the serve.rs teardown: the session's keys are
    /// released from the device by the caller (§12, §22).
    fn model_step(model: &mut Model, device: &MockKeyDevice, op: Op) -> bool {
        let alive = match op {
            Op::Hello => {
                if model.hello {
                    false // duplicate HELLO closes the session
                } else {
                    model.hello = true;
                    true
                }
            }
            Op::OpenSession => {
                if !model.hello || model.open {
                    false // Handshake gate or duplicate OPEN_SESSION
                } else {
                    model.open = true;
                    true
                }
            }
            Op::KeyDown(code) => {
                if !model.open
                    || !device.is_capable(code)
                    || model.held.contains(&code)
                    || model.held.len() >= device.max_held
                {
                    // Not open / UnknownKey / duplicate down / rollover:
                    // each closes the session without touching the ledger.
                    false
                } else {
                    model.held.insert(code);
                    true
                }
            }
            Op::KeyUp(code) => {
                if !model.open || !device.is_capable(code) || !model.held.contains(&code) {
                    false // Not open / UnknownKey / up without down
                } else {
                    model.held.remove(&code);
                    true
                }
            }
            Op::KeyRepeat(code) => {
                model.open && device.is_capable(code) && model.held.contains(&code)
            }
            Op::ReleaseAll => {
                if model.open {
                    model.held.clear();
                    true
                } else {
                    false
                }
            }
            Op::Ping(_) => true,
            // InvalidDown: an out-of-capability code never touches the ledger
            // and always closes the session (UnknownKey), open or not.
            // Disconnect: session teardown; handled uniformly below.
            Op::InvalidDown | Op::Disconnect => false,
        };
        if !alive {
            // The caller (serve.rs) releases exactly this session's keys and
            // the session is removed (§12, §22).
            model.held.clear();
            model.hello = false;
            model.open = false;
        }
        alive
    }

    /// Assert the strong invariants (§22, §87): session-held == model-held,
    /// held ⊆ capability set, and the device's derived state agrees.
    fn assert_invariants(s: &ClientSession, model: &Model, d: &MockKeyDevice) {
        let session_held: std::collections::BTreeSet<u16> = s.held_keys().iter().copied().collect();
        assert_eq!(
            session_held, model.held,
            "session ledger diverged from the model"
        );
        for &code in s.held_keys() {
            assert!(d.is_capable(code), "held key {code} outside capability set");
            assert!(
                d.is_held(code),
                "session holds {code} but the device does not"
            );
        }
        assert!(
            s.held_keys().len() <= s.max_held_keys,
            "held keys exceed the session bound"
        );
    }

    /// The random generator: from any seed, produce a bounded op sequence.
    fn random_op(rng: &mut u64, capable: &[u16]) -> Op {
        match next_rand(rng) % 11 {
            0 => Op::Hello,
            1 => Op::OpenSession,
            2 => Op::KeyDown(capable[(next_rand(rng) as usize) % capable.len()]),
            3 => Op::KeyUp(capable[(next_rand(rng) as usize) % capable.len()]),
            4 => Op::KeyRepeat(capable[(next_rand(rng) as usize) % capable.len()]),
            5 => Op::ReleaseAll,
            6 => Op::Ping(next_rand(rng)),
            7 => Op::InvalidDown,
            _ => Op::Disconnect,
        }
    }

    /// Drive one op through the real implementation.
    fn drive(s: &mut ClientSession, d: &mut MockKeyDevice, op: Op) -> bool {
        let msg = match op {
            Op::Hello => Message::Hello {
                version: PROTOCOL_VERSION,
                client_name: "prop".into(),
            },
            Op::OpenSession => Message::OpenSession,
            Op::KeyDown(c) => Message::KeyDown(c),
            Op::KeyUp(c) => Message::KeyUp(c),
            Op::KeyRepeat(c) => Message::KeyRepeat(c),
            Op::ReleaseAll => Message::ReleaseAll,
            Op::Ping(n) => Message::Ping(n as u32),
            Op::InvalidDown => Message::KeyDown(0x2ff),
            Op::Disconnect => {
                let keys = s.drain_held();
                let errors = d.release_keys(&keys);
                assert!(errors.is_empty());
                return false; // session removed by the caller
            }
        };
        match process_message(s, d, msg) {
            Outcome::Keep | Outcome::Reply(_) => true,
            Outcome::ReplyAndClose(_) | Outcome::Close => {
                // Mirror serve.rs: a closed session releases exactly its
                // keys before removal (§12, §22).
                let keys = s.drain_held();
                let errors = d.release_keys(&keys);
                assert!(errors.is_empty(), "closed session keys must release");
                false
            }
        }
    }

    #[test]
    fn randomized_op_sequences_preserve_invariants() {
        let mut rng: u64 = 0xC0FF_EE00_CAFE_F00D;
        // Under Miri the interpreter is ~1000× slower; the same invariants
        // are exercised on a smaller sample (§86).
        let rounds: u64 = if cfg!(miri) { 20 } else { 500 };
        for round in 0..rounds {
            let mut s = session();
            let mut d = MockKeyDevice::new();
            let mut model = Model::default();
            let capable = d.capability_codes().to_vec();

            // Prime the handshake (both model and implementation) so most of
            // the sequence exercises real key state.
            model_step(&mut model, &d, Op::Hello);
            model_step(&mut model, &d, Op::OpenSession);
            assert!(drive(&mut s, &mut d, Op::Hello));
            assert!(drive(&mut s, &mut d, Op::OpenSession));
            assert_invariants(&s, &model, &d);

            let steps = 30 + (next_rand(&mut rng) % 70) as usize;
            for _ in 0..steps {
                let op = random_op(&mut rng, &capable);
                let model_alive = model_step(&mut model, &d, op);
                let impl_alive = drive(&mut s, &mut d, op);
                assert!(
                    model_alive == impl_alive,
                    "round {round}: model/impl disagree on {op:?}\n  model_alive={model_alive} impl_alive={impl_alive}\n  model: hello={} open={} held={:?}\n  impl:  hello={} open={} held={:?}\n  device_events={:?}",
                    model.hello,
                    model.open,
                    model.held,
                    s.hello_received,
                    s.session_open,
                    s.held_keys(),
                    d.events
                );
                if !impl_alive {
                    // Session torn down: reset both sides identically.
                    s = session();
                    model = Model::default();
                    model_step(&mut model, &d, Op::Hello);
                    model_step(&mut model, &d, Op::OpenSession);
                    assert!(drive(&mut s, &mut d, Op::Hello));
                    assert!(drive(&mut s, &mut d, Op::OpenSession));
                }
                assert_invariants(&s, &model, &d);
            }

            // §87: release_all always empties state — drive the model and the
            // implementation with the same op, then prove both are empty.
            assert!(model_step(&mut model, &d, Op::ReleaseAll));
            let out = process_message(&mut s, &mut d, Message::ReleaseAll);
            assert!(matches!(out, Outcome::Reply(_)));
            assert_invariants(&s, &model, &d);
            assert!(s.held_keys().is_empty());
            assert!(model.held.is_empty());
            for &code in &d.capabilities {
                assert!(!d.is_held(code));
            }
        }
    }

    #[test]
    fn identical_sequences_are_deterministic() {
        // §88: the same op sequence on fresh state must produce the identical
        // device event log (state transitions are deterministic).
        let mut rng: u64 = 0xDEAD_BEEF_1234_5678;
        let mut ops = Vec::new();
        let d0 = MockKeyDevice::new();
        let capable = d0.capability_codes().to_vec();
        for _ in 0..200 {
            ops.push(random_op(&mut rng, &capable));
        }

        let run = |ops: &[Op]| -> Vec<(u16, i32)> {
            let mut s = session();
            let mut d = MockKeyDevice::new();
            for &op in ops {
                if !drive(&mut s, &mut d, op) {
                    s = session();
                }
            }
            d.events
        };
        assert_eq!(run(&ops), run(&ops), "state machine must be deterministic");
    }

    #[test]
    fn invalid_ops_never_touch_the_device() {
        // §87: invalid key operations must be rejected before the kernel path
        // — the device event log stays untouched by them.
        for bad in [0u16, 0x2ff, u16::MAX, 0x100, 0x300] {
            let mut s = session();
            let mut d = MockKeyDevice::new();
            handshake(&mut s, &mut d);
            let before = d.events.len();
            let out = process_message(&mut s, &mut d, Message::KeyDown(bad));
            assert!(matches!(
                out,
                Outcome::ReplyAndClose(Message::Error(ErrorCode::UnknownKey, _))
            ));
            assert_eq!(d.events.len(), before, "code {bad} reached the device");
            assert!(s.held_keys().is_empty());
        }
        // Duplicate down / up-without-down / repeat-without-down: no device
        // events, no ledger change.
        let mut s = session();
        let mut d = MockKeyDevice::new();
        handshake(&mut s, &mut d);
        let a = code(PhysicalKey::A);
        let b = code(PhysicalKey::B);
        process_message(&mut s, &mut d, Message::KeyDown(a));
        let before = d.events.len();
        for msg in [
            Message::KeyDown(a),
            Message::KeyUp(b),
            Message::KeyRepeat(b),
        ] {
            let out = process_message(&mut s, &mut d, msg);
            assert!(matches!(out, Outcome::ReplyAndClose(_)));
        }
        assert_eq!(d.events.len(), before);
        assert!(
            s.held_keys().iter().copied().eq([a].into_iter()),
            "ledger unchanged by invalid ops"
        );
    }
}
