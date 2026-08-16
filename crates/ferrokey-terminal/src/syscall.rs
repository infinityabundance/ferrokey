//! The single raw syscall the terminal crate needs: `TIOCSWINSZ`.
//!
//! nix's ioctl macros generate `unsafe fn`, so the crate policy is
//! `deny(unsafe_code)` (see `Cargo.toml`) with **exactly this module**
//! exempted and audited — §82 of the terminal addendum: any `unsafe` the
//! trusted/sandboxed code requires must be isolated into a tiny module and
//! document its preconditions, postconditions, memory invariants, FD
//! ownership, threading assumptions and error behaviour.
//!
//! # Safety contract
//!
//! * **Preconditions:** `fd` is an open master end of a PTY owned by the
//!   caller; `winsize` points to a valid, correctly aligned `libc::winsize`.
//! * **Postconditions:** on success the kernel has updated the PTY window
//!   size and delivered SIGWINCH to the child's foreground process group;
//!   the calling process's memory is untouched.
//! * **Memory invariants:** the pointer is valid for the duration of the
//!   syscall only; the kernel never retains it. `libc::winsize` is a
//!   `[u16; 4]`-shaped, ABI-stable struct.
//! * **FD ownership:** `fd` is borrowed for the call; never closed,
//!   duplicated or retained.
//! * **Threading assumptions:** a single `ioctl(2)` syscall; no locks are
//!   taken inside the `unsafe` block.
//! * **Error behaviour:** `Err(Errno)` is returned exactly as the kernel
//!   reports it; no state is modified on failure.

#![allow(unsafe_code)]

use nix::errno::Errno;
use nix::libc;
use std::os::fd::RawFd;

/// `ioctl(fd, TIOCSWINSZ, &winsize)`.
///
/// # Safety
///
/// See the module-level contract. The caller must pass an fd it owns for an
/// open PTY master and a pointer to a valid `libc::winsize`.
pub fn tiocswinsz(fd: RawFd, winsize: &libc::winsize) -> Result<(), Errno> {
    // SAFETY: contract above — `winsize` is a valid, aligned `libc::winsize`
    // for the duration of the call; `fd` is the caller's open PTY master;
    // `TIOCSWINSZ` writes the struct into the kernel's pty state.
    let rc = unsafe {
        libc::ioctl(
            fd,
            libc::TIOCSWINSZ,
            std::ptr::from_ref::<libc::winsize>(winsize),
        )
    };
    if rc == -1 {
        Err(Errno::last())
    } else {
        Ok(())
    }
}

/// `_exit(code)` — terminates the calling process without flushing stdio or
/// running destructors. Used by the fork child between fork and exec, where
/// only async-signal-safe operations are allowed.
///
/// # Safety
///
/// `_exit` never returns; the code path must not rely on any destructors or
/// buffered output running afterwards.
pub fn exit_now(code: i32) -> ! {
    // SAFETY: `_exit` is always safe to call (it is async-signal-safe and
    // cannot fail); it terminates the process immediately.
    unsafe { libc::_exit(code) }
}

/// `write(2, …)` to stderr (fd 2) — the only correct way for the fork child
/// to report a failure between fork and exec.
///
/// `eprintln!`/`log!` take the stdio lock, which another thread may still
/// hold at fork time: the post-fork child would deadlock before reporting
/// the error. A raw `write` is async-signal-safe, allocation-free and
/// lock-free.
///
/// # Safety
///
/// See the module-level contract. `bytes` is a valid, immutable slice for
/// the duration of the call; fd 2 is the calling process's stderr.
pub fn write_stderr(bytes: &[u8]) {
    // SAFETY: contract above — `bytes` points to a valid byte slice for the
    // duration of the syscall only (the kernel never retains it); `write` is
    // async-signal-safe and returns without touching program state. Errors
    // are ignored: this is a best-effort diagnostic on a path that is about
    // to `_exit`.
    unsafe {
        libc::write(2, bytes.as_ptr().cast::<libc::c_void>(), bytes.len());
    }
}

/// Test-only: `dup2(src, dst)` for the fd-2 redirection test in `child.rs`.
/// Exists here because `nix` 0.31's `dup2` takes an `OwnedFd` for the
/// destination, which cannot represent the process's own stderr safely.
///
/// # Safety
///
/// Both fds must be open in the calling process; `dst`'s previous target is
/// closed by the kernel. The caller must restore `dst` before any fallible
/// operation (a failed test must not leave the harness's stderr redirected).
#[cfg(test)]
pub(crate) fn dup2_for_tests(src: RawFd, dst: RawFd) {
    // SAFETY: contract above — a single `dup2(2)` syscall, no retained state.
    unsafe {
        libc::dup2(src, dst);
    }
}

/// `fork()` — the raw fork wrapper.
///
/// # Safety
///
/// `fork` is only safe when the forking process's threads hold no locks that
/// the child might touch before `exec`, and the child execs immediately using
/// only async-signal-safe operations. The caller guarantees this (see
/// [`crate::child`]): all `CString`s are prepared before the fork, and the
/// child path performs only syscalls.
pub fn fork() -> Result<nix::unistd::ForkResult, Errno> {
    // SAFETY: contract above — the caller is single-threaded at this point
    // and execs immediately after the fork.
    unsafe { nix::unistd::fork() }
}

/// `execv(path, argv)` — like [`execvp`] but with a **pre-resolved absolute
/// path** (no PATH search, which can allocate inside glibc after a fork).
/// The caller resolves the path in the parent.
///
/// # Safety
///
/// Same contract as [`execvp`].
pub fn execv(path: &std::ffi::CStr, argv0: &std::ffi::CStr) -> Result<(), Errno> {
    let argv: [*const libc::c_char; 2] = [argv0.as_ptr(), std::ptr::null()];
    // SAFETY: see execvp.
    unsafe { libc::execv(path.as_ptr(), argv.as_ptr()) };
    Err(Errno::last())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::{PtyPair, Winsize};

    #[test]
    fn tiocswinsz_updates_the_pty() {
        let pty = PtyPair::open(Winsize {
            rows: 24,
            cols: 80,
            ..Winsize::default()
        })
        .unwrap();
        let ws = libc::winsize {
            ws_row: 41,
            ws_col: 101,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        tiocswinsz(pty.master_fd(), &ws).unwrap();
        // Reading it back through nix's safe wrapper confirms the write.
        let mut back = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { libc::ioctl(pty.master_fd(), libc::TIOCGWINSZ, &mut back) };
        assert_eq!(rc, 0);
        assert_eq!(back.ws_row, 41);
        assert_eq!(back.ws_col, 101);
    }
}
