//! Event emission for the virtual keyboard — pure safe code.
//!
//! The runtime event path writes raw `struct input_event` bytes to the
//! pre-created uinput fd. This is deliberately **not** `ioctl`-based: after
//! the seccomp freeze, the runtime performs no `ioctl` at all (§14, §61).
//!
//! # Wire layout (kernel `struct input_event`, uapi/linux/input.h)
//!
//! ```text
//! ┌───────────────────────────────┬─────────┬─────────┬──────────┐
//! │ struct timeval time (16 B)    │ u16 ty  │ u16 cod │ s32 val  │
//! └───────────────────────────────┴─────────┴─────────┴──────────┘
//!                                  24 bytes, no padding on the
//!                                  supported architectures
//! ```
//!
//! The timeval is left zero: per the kernel's own behavior (and the `evdev`
//! crate's documentation for `InputEvent::new`), the kernel stamps events
//! with its own timestamp when delivering them to reading clients.
//!
//! # Event-class discipline (§19)
//!
//! Ferrokey's runtime kernel interface is keyboard-specific: only `EV_KEY`
//! events are ever constructed here, plus the terminating `EV_SYN`/`SYN_REPORT`
//! that uinput requires. No other event class (`EV_REL`, `EV_ABS`, `EV_MSC`,
//! `EV_SW`, `EV_LED`, `EV_SND`, `EV_FF`) is reachable from this module.

use std::io;
use std::os::fd::BorrowedFd;

/// `EV_KEY` — key press/release/repeat events.
pub const EV_KEY: u16 = 0x01;
/// `EV_SYN` — synchronization events.
pub const EV_SYN: u16 = 0x00;
/// `SYN_REPORT` — end of an event batch.
pub const SYN_REPORT: u16 = 0x00;

/// One `EV_KEY` event with a kernel-valid value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// The linux input key code (must belong to the immutable capability set).
    pub code: u16,
    /// 1 = press, 0 = release, 2 = autorepeat.
    pub value: i32,
}

impl KeyEvent {
    /// Construct a validated key event.
    ///
    /// # Preconditions
    /// * `code` must have passed the protocol-boundary validation and belong
    ///   to the immutable capability set (§20, §21).
    /// * `value` must be exactly 0, 1 or 2.
    pub fn new(code: u16, value: i32) -> Self {
        debug_assert!(matches!(value, 0..=2), "invalid EV_KEY value {value}");
        KeyEvent { code, value }
    }

    /// Encode as the 24-byte `struct input_event` image.
    fn encode(self) -> [u8; 24] {
        let mut buf = [0u8; 24]; // timeval zeroed: kernel stamps delivery time
        buf[16..18].copy_from_slice(&EV_KEY.to_le_bytes());
        buf[18..20].copy_from_slice(&self.code.to_le_bytes());
        buf[20..24].copy_from_slice(&self.value.to_le_bytes());
        buf
    }
}

/// The terminating `SYN_REPORT` event image (type 0, code 0, value 0).
fn syn_report() -> [u8; 24] {
    [0u8; 24]
}

/// Write a batch of `EV_KEY` events plus the trailing `SYN_REPORT` to the
/// device fd in a single `write(2)`-style operation.
///
/// This is the **only** kernel write path Ferrokey uses after the security
/// freeze.
pub fn write_batch(fd: BorrowedFd<'_>, events: &[KeyEvent]) -> io::Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let mut buf = Vec::with_capacity(events.len() * 24 + 24);
    for ev in events {
        buf.extend_from_slice(&ev.encode());
    }
    buf.extend_from_slice(&syn_report());
    write_all(fd, &buf)
}

/// Write a single key event batch (used by the tap/ledger paths).
pub fn write_one(fd: BorrowedFd<'_>, code: u16, value: i32) -> io::Result<()> {
    write_batch(fd, &[KeyEvent::new(code, value)])
}

/// `write(2)`-all loop: retries on `EINTR`, follows partial writes.
///
/// # Note
/// A hostile or wedged peer cannot cause this to allocate: `buf` is sized by
/// the caller from a bounded capability list (max `MAX_HELD_KEYS` entries).
fn write_all(fd: BorrowedFd<'_>, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        match nix::unistd::write(fd, buf) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "uinput write returned 0",
                ));
            }
            Ok(n) => buf = &buf[n..],
            Err(nix::errno::Errno::EINTR) => {}
            Err(e) => return Err(io::Error::from(e)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd as _;

    #[test]
    fn event_layout_matches_kernel_struct() {
        // struct input_event = { timeval(16) + u16 type + u16 code + i32 value }
        let ev = KeyEvent::new(30, 1);
        let bytes = ev.encode();
        assert_eq!(bytes.len(), 24);
        // timeval zeroed
        assert_eq!(&bytes[..16], &[0u8; 16]);
        // type = EV_KEY
        assert_eq!(&bytes[16..18], &[0x01, 0x00]);
        // code = 30 LE
        assert_eq!(&bytes[18..20], &[30, 0x00]);
        // value = 1 LE
        assert_eq!(&bytes[20..24], &[1, 0, 0, 0]);
    }

    #[test]
    fn key_event_values_are_closed() {
        // The closed value set: 0 (up), 1 (down), 2 (repeat).
        let down = KeyEvent::new(30, 1);
        let up = KeyEvent::new(30, 0);
        let repeat = KeyEvent::new(30, 2);
        assert_eq!(down.value, 1);
        assert_eq!(up.value, 0);
        assert_eq!(repeat.value, 2);
    }

    #[test]
    fn write_batch_builds_events_plus_syn() {
        // Verify via a socketpair: write the batch to one end, read the exact
        // bytes from the other. No kernel uinput needed.
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let events = [KeyEvent::new(30, 1), KeyEvent::new(30, 0)];
        write_batch(a.as_fd(), &events).unwrap();
        drop(a);
        let mut received = Vec::new();
        let mut chunk = [0u8; 128];
        let mut b = b;
        loop {
            match std::io::Read::read(&mut b, &mut chunk) {
                Ok(0) => break,
                Ok(n) => received.extend_from_slice(&chunk[..n]),
                Err(e) => panic!("read failed: {e}"),
            }
        }
        assert_eq!(received.len(), 3 * 24);
        // Two EV_KEY events then the zeroed SYN_REPORT.
        assert_eq!(&received[..24], &KeyEvent::new(30, 1).encode());
        assert_eq!(&received[24..48], &KeyEvent::new(30, 0).encode());
        assert_eq!(&received[48..72], &[0u8; 24]);
    }

    #[test]
    fn empty_batch_writes_nothing() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        write_batch(a.as_fd(), &[]).unwrap();
        drop(a);
        let mut chunk = [0u8; 8];
        let mut b = b;
        assert_eq!(std::io::Read::read(&mut b, &mut chunk).unwrap(), 0);
    }
}
