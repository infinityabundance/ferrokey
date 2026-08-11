//! Peer identity via `SO_PEERCRED`.
//!
//! `ferrokeyd` must know *who* is talking to it. On Linux, Unix-domain
//! sockets provide `SO_PEERCRED`: the kernel reports the effective uid, gid
//! and pid of the peer **without any spoofing possible** — the values come
//! from the kernel's socket metadata, not from anything the client can fake.

use std::os::unix::net::UnixStream;

/// The kernel-reported identity of the peer on a Unix socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIdentity {
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
}

impl std::fmt::Display for PeerIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "uid={} gid={} pid={}", self.uid, self.gid, self.pid)
    }
}

/// Obtain the peer credentials of a connected Unix socket.
///
/// Uses `SO_PEERCRED`; on non-Linux platforms this returns an error
/// (Ferrokey targets Linux only).
pub fn peer_identity(stream: &UnixStream) -> std::io::Result<PeerIdentity> {
    use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
    let cred = getsockopt(stream, PeerCredentials).map_err(std::io::Error::from)?;
    Ok(PeerIdentity {
        uid: cred.uid() as u32,
        gid: cred.gid() as u32,
        pid: cred.pid() as u32,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_identity_reflects_caller() {
        // A connected socket pair: each end reports the *other* end's
        // credentials, which are this process's.
        let (a, b) = UnixStream::pair().unwrap();
        let id_a = peer_identity(&b).unwrap();
        let id_b = peer_identity(&a).unwrap();
        let me = nix::unistd::geteuid().as_raw();
        let my_gid = nix::unistd::getegid().as_raw();
        assert_eq!(id_a.uid, me);
        assert_eq!(id_a.gid, my_gid);
        assert_eq!(id_b.uid, me);
        assert_eq!(id_b.gid, my_gid);
        assert_eq!(id_a.pid, std::process::id());
    }
}
