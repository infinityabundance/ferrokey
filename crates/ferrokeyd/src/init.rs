//! The bootstrap component (`ferrokeyd init`) — the *tiny initialization
//! TCB* (§15, §16).
//!
//! Its job is exactly:
//!
//! ```text
//! open /dev/uinput
//! configure the exact capability set
//! create the device
//! verify creation (identity + capability bitmap)
//! transfer the fd to the runtime
//! exit
//! ```
//!
//! It must **not**: parse UI protocol, render anything, touch the network,
//! load plugins, parse layouts, run prediction, interpret scripts, or remain
//! resident. It holds temporary root/uinput authority for a few milliseconds
//! and then exits; the process that later parses hostile IPC (`serve`) never
//! possesses uinput *configuration* authority (§15, §16, §14).

use crate::fds;
use ferrokey_uinput::{DeviceOptions, UinputDevice};
use std::io;
use std::os::fd::RawFd;

/// Errors from the bootstrap phase.
#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("cannot create the uinput device: {0}")]
    Device(#[from] ferrokey_uinput::UinputError),
    #[error("cannot transfer the device fd: {0}")]
    Transfer(io::Error),
}

/// Run the bootstrap phase.
///
/// # Preconditions
/// * `handoff_fd` is one end of the private socketpair whose other end is
///   held by the runtime-to-be.
/// * The process has the authority to open `/dev/uinput` (root or the
///   dedicated uinput group).
///
/// # Postconditions
/// * On `Ok`, exactly one virtual keyboard exists, was verified, and its fd
///   was transferred; the caller exits immediately.
pub fn run(handoff_fd: RawFd, device_name: &str, max_held_keys: usize) -> Result<(), InitError> {
    let options = DeviceOptions {
        name: device_name.to_string(),
        max_held_keys,
    };
    let device = UinputDevice::create(options)?;
    let raw = device.raw_fd();
    log::info!(
        "init: created and verified virtual keyboard '{}' (fd {raw})",
        device.name()
    );
    fds::send_fd(handoff_fd, raw).map_err(InitError::Transfer)?;
    log::info!("init: transferred device fd to runtime");
    Ok(())
}
