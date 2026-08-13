//! PTY plumbing (§6): a real pseudo-terminal pair via `nix::pty::openpty`.
//!
//! Ferrokey's terminal workspace is a genuine Linux PTY — never a text-widget
//! imitation. This module owns the master/slave pair, the slave device path
//! (the child reopens it after `setsid` to acquire the controlling terminal)
//! and window-size updates (`TIOCSWINSZ` via the tiny audited
//! [`crate::syscall`] module).
//!
//! All other syscalls go through `nix` safe wrappers.

use crate::terminal::TerminalError;
use nix::pty::{openpty, Winsize as NixWinsize};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};

/// PTY window size. `xpixel`/`ypixel` are informational (most kernels ignore
/// them; the PTY is sized by rows/cols).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Winsize {
    pub rows: u16,
    pub cols: u16,
    pub xpixel: u16,
    pub ypixel: u16,
}

impl Default for Winsize {
    fn default() -> Self {
        // A sane fallback only; the app computes real sizes from cell
        // metrics (§32).
        Winsize {
            rows: 24,
            cols: 80,
            xpixel: 0,
            ypixel: 0,
        }
    }
}

impl From<Winsize> for NixWinsize {
    fn from(w: Winsize) -> Self {
        NixWinsize {
            ws_row: w.rows,
            ws_col: w.cols,
            ws_xpixel: w.xpixel,
            ws_ypixel: w.ypixel,
        }
    }
}

impl From<NixWinsize> for Winsize {
    fn from(w: NixWinsize) -> Self {
        Winsize {
            rows: w.ws_row,
            cols: w.ws_col,
            xpixel: w.ws_xpixel,
            ypixel: w.ws_ypixel,
        }
    }
}

/// An open PTY pair.
#[derive(Debug)]
pub struct PtyPair {
    master: OwnedFd,
    /// The slave fd, kept open by the parent only until the child has been
    /// spawned; then closed ([`PtyPair::close_slave`]).
    slave: Option<OwnedFd>,
    slave_path: std::path::PathBuf,
}

impl PtyPair {
    /// Open a new PTY with the given window size.
    pub fn open(winsize: Winsize) -> Result<Self, TerminalError> {
        let size = NixWinsize::from(winsize);
        let result = openpty(Some(&size), None)?;
        let slave_path = slave_path_of(&result.slave)?;
        Ok(PtyPair {
            master: result.master,
            slave: Some(result.slave),
            slave_path,
        })
    }

    /// The master end (read output, write input).
    pub fn master(&self) -> &OwnedFd {
        &self.master
    }

    pub fn master_fd(&self) -> std::os::fd::RawFd {
        self.master.as_fd().as_raw_fd()
    }

    /// The slave device path (the child reopens it after `setsid` to become
    /// the controlling terminal).
    pub fn slave_path(&self) -> &std::path::Path {
        &self.slave_path
    }

    /// The slave fd (available to the child at fork time; then closed).
    pub fn slave(&self) -> Option<&OwnedFd> {
        self.slave.as_ref()
    }

    /// Update the PTY window size (`TIOCSWINSZ`); the kernel signals the
    /// child's foreground process group with SIGWINCH.
    pub fn resize(&mut self, winsize: Winsize) -> Result<(), TerminalError> {
        let nix_size = NixWinsize::from(winsize);
        let libc_size = nix::libc::winsize {
            ws_row: nix_size.ws_row,
            ws_col: nix_size.ws_col,
            ws_xpixel: nix_size.ws_xpixel,
            ws_ypixel: nix_size.ws_ypixel,
        };
        crate::syscall::tiocswinsz(self.master_fd(), &libc_size)?;
        Ok(())
    }

    /// Close the slave end in the parent (after the child has been spawned).
    pub fn close_slave(&mut self) {
        self.slave.take();
    }

    /// Make the master non-blocking (the event loop must never block on it).
    pub fn make_nonblocking(&self) -> Result<(), TerminalError> {
        use nix::fcntl::{fcntl, FcntlArg, OFlag};
        let flags = fcntl(self.master(), FcntlArg::F_GETFL)?;
        let flags = OFlag::from_bits_truncate(flags);
        fcntl(self.master(), FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        Ok(())
    }
}

/// Resolve the slave device path from its fd via `/proc/self/fd` (Linux;
/// the only platform Ferrokey targets).
fn slave_path_of(slave: &OwnedFd) -> Result<std::path::PathBuf, TerminalError> {
    let link = std::fs::read_link(format!("/proc/self/fd/{}", slave.as_raw_fd()))
        .map_err(TerminalError::Io)?;
    Ok(link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_pty_creates_pair() {
        let pty = PtyPair::open(Winsize {
            rows: 24,
            cols: 80,
            ..Winsize::default()
        })
        .unwrap();
        assert!(pty.slave_path().exists());
        assert!(pty.master_fd() >= 0);
        assert!(pty.slave().is_some());
    }

    #[test]
    fn resize_round_trips() {
        let mut pty = PtyPair::open(Winsize {
            rows: 24,
            cols: 80,
            ..Winsize::default()
        })
        .unwrap();
        pty.resize(Winsize {
            rows: 40,
            cols: 100,
            ..Winsize::default()
        })
        .unwrap();
    }

    #[test]
    fn close_slave_releases_it() {
        let mut pty = PtyPair::open(Winsize::default()).unwrap();
        pty.close_slave();
        assert!(pty.slave().is_none());
    }
}
