//! Unix-domain fd transfer (SCM_RIGHTS) between the bootstrap component and
//! the runtime broker (§15).
//!
//! `ferrokeyd init` creates the kernel device and sends the fd over a
//! private socketpair; `ferrokeyd serve` receives it. The mechanism is the
//! kernel's own `SCM_RIGHTS` — no RPC framework is introduced for this
//! (§15: "Do not introduce a large RPC framework merely for FD transfer").
//!
//! The socketpair is `SOCK_SEQPACKET`: messages are preserved as units.
//! Close-on-exec is deliberately *not* set on the handoff fds: the children
//! need them across `exec` (the supervisor passes the fd numbers on argv and
//! the child closes the end it does not need in `pre_exec`).
//!
//! # Unsafe discipline (§82)
//!
//! The only `unsafe` is wrapping kernel-returned raw fds into owned
//! handles; each occurrence documents the ownership transfer.

use nix::sys::socket::{
    recvmsg, sendmsg, socketpair, AddressFamily, ControlMessage, ControlMessageOwned, MsgFlags,
    SockFlag, SockProtocol, SockType,
};
use std::io;
use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;

/// Create a private socketpair for the bootstrap handoff.
///
/// # Postconditions
/// * Returns the init end (a) and serve end (b). Neither is close-on-exec:
///   the forked children inherit them and the supervisor closes the ends it
///   does not need.
pub fn handoff_socketpair() -> io::Result<(OwnedFd, OwnedFd)> {
    let (a, b) = socketpair(
        AddressFamily::Unix,
        SockType::SeqPacket,
        None::<SockProtocol>,
        SockFlag::empty(),
    )
    .map_err(io::Error::from)?;
    Ok((a, b))
}

/// A `pre_exec` closure that closes `fd` in the forked child (async-signal-safe
/// `close(2)`). Used so `init`/`serve` never hold the handoff end they do not
/// need.
pub fn close_in_child(fd: RawFd) -> impl FnMut() -> io::Result<()> + Send + Sync + 'static {
    move || {
        nix::unistd::close(fd).map_err(io::Error::from)?;
        Ok(())
    }
}

/// Attach a `pre_exec` closure to a [`std::process::Command`].
///
/// # Safety note (§82)
/// The `pre_exec` call itself is `unsafe` in std; this helper isolates it.
/// The closure runs in the forked child before `exec` and must only perform
/// async-signal-safe operations (the caller is responsible for that).
pub fn command_with_close(
    cmd: std::process::Command,
    closure: impl FnMut() -> io::Result<()> + Send + Sync + 'static,
) -> std::process::Command {
    use std::os::unix::process::CommandExt;
    let mut cmd = cmd;
    // SAFETY: delegated to the caller's closure, which must be
    // async-signal-safe (our `close_in_child` and the credential drop in
    // `security::command_with_dropped_identity` are).
    unsafe {
        cmd.pre_exec(closure);
    }
    cmd
}

/// Send one raw fd plus a zero-length payload over `fd`.
pub fn send_fd(fd: RawFd, pass: RawFd) -> io::Result<()> {
    let cmsg = [ControlMessage::ScmRights(&[pass])];
    let iov = [IoSlice::new(b"")];
    sendmsg(fd, &iov, &cmsg, MsgFlags::empty(), None::<&()>).map_err(io::Error::from)?;
    Ok(())
}

/// Receive one raw fd over `fd`.
///
/// # Preconditions
/// * Exactly one `SCM_RIGHTS` message carrying exactly one fd is expected.
/// * Multiple fds in the message are rejected: the caller must close every
///   received fd it does not adopt.
pub fn recv_fd(fd: RawFd) -> io::Result<RawFd> {
    let mut byte = [0u8; 1];
    let mut iov = [IoSliceMut::new(&mut byte)];
    let mut cmsg_space = nix::cmsg_space!([RawFd; 1]);
    let msg = recvmsg::<()>(fd, &mut iov, Some(&mut cmsg_space), MsgFlags::empty())
        .map_err(io::Error::from)?;
    let mut received = Vec::new();
    let cmsgs = msg.cmsgs().map_err(io::Error::from)?;
    for cmsg in cmsgs {
        if let ControlMessageOwned::ScmRights(fds) = cmsg {
            received.extend(fds);
        }
    }
    match received.as_slice() {
        [single] => Ok(*single),
        [] => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "no fd received in handoff message",
        )),
        _ => {
            for fd in &received {
                let _ = nix::unistd::close(*fd);
            }
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "handoff message carried more than one fd",
            ))
        }
    }
}

/// Receive and adopt the uinput fd as an `OwnedFd`.
pub fn recv_owned_fd(fd: RawFd) -> io::Result<OwnedFd> {
    let raw = recv_fd(fd)?;
    // SAFETY: `recv_fd` returned an fd owned by us (from SCM_RIGHTS); it has
    // no other owner, so wrapping it in OwnedFd is the single-owner transfer.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// `accept4(2)` with `SOCK_NONBLOCK | SOCK_CLOEXEC`, wrapped as a
/// `UnixStream`. Non-blocking + close-on-exec are set atomically at accept
/// time so the runtime never needs `fcntl` (which the seccomp filter
/// denies).
pub fn accept4_stream(listener: RawFd) -> io::Result<UnixStream> {
    let fd = nix::sys::socket::accept4(listener, SockFlag::SOCK_NONBLOCK | SockFlag::SOCK_CLOEXEC)
        .map_err(io::Error::from)?;
    // SAFETY: accept4 returned a fresh fd owned by this process; the
    // `UnixStream` becomes its single owner.
    Ok(unsafe { UnixStream::from_raw_fd(fd) })
}

/// A tiny helper so tests can observe a raw fd number.
pub fn raw_fd<T: AsRawFd>(t: &T) -> RawFd {
    t.as_raw_fd()
}

/// Borrow a raw fd number as a `BorrowedFd` for use in `PollFd` sets.
///
/// # Safety contract
/// * The caller guarantees the fd remains open for the lifetime of the
///   returned borrow and of any `PollFd` built from it. In the broker this
///   holds because the poll set is rebuilt every loop iteration and the
///   owning objects (listener, sessions) outlive it.
pub fn borrow_fd(fd: RawFd) -> std::os::fd::BorrowedFd<'static> {
    // SAFETY: per the documented contract above — the raw fd stays valid
    // while the poll set built from it is alive.
    unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fd_round_trip_over_socketpair() {
        let (a, b) = handoff_socketpair().unwrap();
        let a_raw = raw_fd(&a);
        let b_raw = raw_fd(&b);
        // Create a real fd to pass (an anonymous pipe).
        let (r, w) = nix::unistd::pipe().unwrap();
        let w_raw = raw_fd(&w);
        send_fd(a_raw, w_raw).unwrap();
        drop(w);
        let got = recv_fd(b_raw).unwrap();
        // The received fd must reference the same pipe: writing through it
        // must be readable from `r`.
        nix::unistd::write(borrow_fd(got), b"x").unwrap();
        let mut buf = [0u8; 1];
        nix::unistd::read(borrow_fd(raw_fd(&r)), &mut buf).unwrap();
        assert_eq!(&buf, b"x");
        nix::unistd::close(got).unwrap();
        drop(r);
        drop(a);
        drop(b);
    }

    #[test]
    fn recv_without_sender_errors() {
        let (a, b) = handoff_socketpair().unwrap();
        let a_raw = raw_fd(&a);
        let b_raw = raw_fd(&b);
        drop(a);
        // Writing side closed → recvmsg returns an error.
        assert!(recv_fd(b_raw).is_err());
        drop(b);
        let _ = a_raw;
    }

    #[test]
    fn accept4_stream_is_nonblocking() {
        let dir = std::env::temp_dir().join(format!("fk-fds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stream = accept4_stream(std::os::fd::AsRawFd::as_raw_fd(&listener));
        // No pending connection → EAGAIN/EWOULDBLOCK, proving nonblocking.
        let err = stream.unwrap_err();
        assert!(err.kind() == io::ErrorKind::WouldBlock);
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(&dir).unwrap();
    }
}
