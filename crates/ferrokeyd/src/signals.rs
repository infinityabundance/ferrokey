//! Signal handling for the runtime broker.
//!
//! # Unsafe discipline (§82)
//!
//! `sigaction(2)` is `unsafe` in nix because the handler must satisfy
//! async-signal-safety. The handler installed here is a plain `extern "C"`
//! function that only performs a seqcst store to a static `AtomicBool` —
//! async-signal-safe by construction — and the nix wrapper is the only
//! `unsafe` in this module.

use nix::sys::signal::{sigaction, SaFlags, SigAction, SigHandler, SigSet, Signal};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

/// The process-wide stop flag set by the SIGTERM/SIGINT handler.
pub static STOP: AtomicBool = AtomicBool::new(false);

/// Install SIGTERM/SIGINT handling: an async-signal-safe handler that only
/// sets the stop flag. The event loop polls with a timeout and observes it.
///
/// # Postconditions
/// * On `Ok`, SIGTERM and SIGINT are caught and set [`STOP`].
/// * The handler performs no allocation, locking or I/O — it is safe to run
///   in a signal context.
pub fn install() -> io::Result<()> {
    let handler = SigHandler::Handler(signal_stop);
    let action = SigAction::new(handler, SaFlags::SA_RESTART, SigSet::empty());
    // SAFETY: `signal_stop` is async-signal-safe; nix validates the action.
    unsafe { sigaction(Signal::SIGTERM, &action) }.map_err(io::Error::from)?;
    unsafe { sigaction(Signal::SIGINT, &action) }.map_err(io::Error::from)?;
    Ok(())
}

extern "C" fn signal_stop(_sig: i32) {
    STOP.store(true, Ordering::SeqCst);
}
