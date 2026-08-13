//! Tiny, isolated, fully-documented `unsafe` FFI to the Linux uinput ABI.
//!
//! # Why this module exists
//!
//! The kernel's `uinput` device must be *configured and created once*, by the
//! bootstrap component (`ferrokeyd init`), and the resulting file descriptor
//! transferred to the runtime broker (`ferrokeyd serve`). The `evdev` crate
//! deliberately keeps its `VirtualDevice` fd private and performs
//! `UI_DEV_SETUP`/`UI_DEV_CREATE` inside its private constructor — it is
//! impossible to create the device and hand the fd onward with it. Rather
//! than fork a dependency, this module implements the six ioctls Ferrokey
//! actually needs against the stable kernel ABI (`include/uapi/linux/uinput.h`).
//!
//! # Scope discipline (Phase 3 §82)
//!
//! * Every `unsafe` block in the crate lives in this module.
//! * The module performs **configuration-time ioctls only**. The runtime
//!   event path (see [`crate::emit`]) is pure safe code over `write(2)`.
//! * Each function documents its preconditions, postconditions, and the
//!   kernel invariants it relies on.
//!
//! # Trusted Computing Base note
//!
//! This module is part of the *tiny initialization TCB*: it is exercised only
//! by `ferrokeyd init` before any hostile input is accepted, and (read-only,
//! for verification) by `ferrokeyd serve` *before* the seccomp freeze that
//! blocks all further `ioctl`.

use std::io;
use std::os::fd::RawFd;

// The `syscall(2)` numbers are provided by libc (re-exported through nix).
use nix::libc::{self, c_void};

// ---------------------------------------------------------------------------
// ioctl request encoding (uapi/linux/ioctl.h):
//   _IOC(dir, type, nr, size) = (dir << 30) | (size << 16) | (type << 8) | nr
//   _IOC_WRITE = 1, _IOC_READ = 2, _IOC_NONE = 0
// ---------------------------------------------------------------------------

const fn ioc(dir: u32, size: u32, typ: u32, nr: u32) -> libc::c_ulong {
    ((dir << 30) | (size << 16) | (typ << 8) | nr) as libc::c_ulong
}

const fn iow(typ: u32, nr: u32, size: u32) -> libc::c_ulong {
    ioc(1, size, typ, nr)
}

const fn io(typ: u32, nr: u32) -> libc::c_ulong {
    ioc(0, 0, typ, nr)
}

const fn ior(typ: u32, nr: u32, size: u32) -> libc::c_ulong {
    ioc(2, size, typ, nr)
}

const UINPUT_IOCTL_BASE: u32 = b'U' as u32;

/// `UI_SET_EVBIT` — enable an event type (e.g. `EV_KEY`).
pub(crate) const UI_SET_EVBIT: libc::c_ulong = iow(UINPUT_IOCTL_BASE, 100, size_of::<i32>() as u32);
/// `UI_SET_KEYBIT` — enable a key code in the device's capability bitmap.
pub(crate) const UI_SET_KEYBIT: libc::c_ulong =
    iow(UINPUT_IOCTL_BASE, 101, size_of::<i32>() as u32);
/// `UI_DEV_SETUP` — supply the device identity/name (modern creation path).
pub(crate) const UI_DEV_SETUP: libc::c_ulong =
    iow(UINPUT_IOCTL_BASE, 3, size_of::<UinputSetup>() as u32);
/// `UI_DEV_CREATE` — register the device with the input core.
pub(crate) const UI_DEV_CREATE: libc::c_ulong = io(UINPUT_IOCTL_BASE, 1);
/// `UI_GET_SYSNAME` — read back the kernel-assigned `input<N>` name.
pub(crate) const UI_GET_SYSNAME: libc::c_ulong = ior(UINPUT_IOCTL_BASE, 300, 256);

/// `struct input_id` (uapi/linux/input.h) — 8 bytes, no padding on the
/// supported architectures.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InputId {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

/// `struct uinput_setup` (uapi/linux/uinput.h) — 92 bytes:
/// `{ input_id id; char name[80]; u32 ff_effects_max; }`.
///
/// `UINPUT_MAX_NAME_SIZE == 80`, matching the kernel constant.
#[repr(C)]
pub(crate) struct UinputSetup {
    pub id: InputId,
    pub name: [libc::c_char; UINPUT_MAX_NAME_SIZE],
    pub ff_effects_max: u32,
}

pub(crate) const UINPUT_MAX_NAME_SIZE: usize = 80;

impl UinputSetup {
    /// # Preconditions
    /// * `name` must be valid UTF-8 (caller guarantees) and strictly shorter
    ///   than [`UINPUT_MAX_NAME_SIZE`] (the kernel requires space for the NUL
    ///   terminator).
    pub fn new(id: InputId, name: &str) -> Self {
        let mut raw = [0i8; UINPUT_MAX_NAME_SIZE];
        for (dst, src) in raw.iter_mut().zip(name.as_bytes()) {
            *dst = *src as i8;
        }
        UinputSetup {
            id,
            name: raw,
            ff_effects_max: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Raw syscall wrappers.
// ---------------------------------------------------------------------------

fn ioctl_errno(result: libc::c_long) -> io::Result<()> {
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Enable an event bit (`UI_SET_EVBIT`).
///
/// # Safety contract
/// * Precondition: `fd` is a valid open `/dev/uinput` descriptor.
/// * Postcondition: on `Ok`, the kernel has set the event bit in the
///   in-progress device configuration; on `Err`, the device configuration is
///   unchanged.
/// * Kernel invariant: the ioctl must be issued before `UI_DEV_CREATE`.
/// * ABI note: for these bit-setting ioctls the kernel reads the bit number
///   **by value** from the ioctl argument register (`uinput_set_bit(arg, …)`
///   in uinput.c) — it is NOT a pointer, despite the `_IOW(…, int)` size.
pub(crate) fn ui_set_evbit(fd: RawFd, bit: i32) -> io::Result<()> {
    // SAFETY: `fd` is a valid open fd (caller contract). The bit is passed
    // by value as the ioctl argument (see ABI note); no pointer escapes.
    let result = unsafe { libc::syscall(libc::SYS_ioctl, fd, UI_SET_EVBIT, bit as usize) };
    ioctl_errno(result)
}

/// Enable a key bit (`UI_SET_KEYBIT`).
///
/// # Safety contract
/// * Precondition: `fd` is a valid open `/dev/uinput` descriptor.
/// * Postcondition: on `Ok`, the key code is part of the in-progress device
///   capability bitmap; on `Err`, the bitmap is unchanged.
/// * ABI note: as `ui_set_evbit` — the bit number is passed by value.
pub(crate) fn ui_set_keybit(fd: RawFd, bit: i32) -> io::Result<()> {
    // SAFETY: as `ui_set_evbit` — the bit is passed by value in the ioctl
    // argument register, not via a pointer.
    let result = unsafe { libc::syscall(libc::SYS_ioctl, fd, UI_SET_KEYBIT, bit as usize) };
    ioctl_errno(result)
}

/// Supply device identity/name (`UI_DEV_SETUP`).
///
/// # Safety contract
/// * Precondition: `fd` is a valid open `/dev/uinput` descriptor; `setup`
///   points to a valid, fully-initialized `UinputSetup` of the exact kernel
///   layout.
/// * Postcondition: on `Ok`, the kernel has recorded the identity/name for
///   the device about to be created; on `Err`, nothing changed.
pub(crate) fn ui_dev_setup(fd: RawFd, setup: &UinputSetup) -> io::Result<()> {
    // SAFETY: `setup` is a valid reference to a `UinputSetup` whose layout
    // exactly matches `struct uinput_setup` (checked by the ABI test above).
    // The kernel copies the bytes during the call; the reference does not
    // escape. `UI_DEV_SETUP` is a `_IOW` request carrying the struct size.
    let result = unsafe {
        libc::syscall(
            libc::SYS_ioctl,
            fd,
            UI_DEV_SETUP,
            std::ptr::from_ref::<UinputSetup>(setup),
        )
    };
    ioctl_errno(result)
}

/// Register the device with the input core (`UI_DEV_CREATE`).
///
/// # Safety contract
/// * Precondition: `fd` is a valid open `/dev/uinput` descriptor whose
///   configuration has been fully supplied.
/// * Postcondition: on `Ok`, the virtual device is live and visible in
///   `/proc/bus/input/devices`; on `Err`, no device was created.
pub(crate) fn ui_dev_create(fd: RawFd) -> io::Result<()> {
    // SAFETY: no pointer argument; the syscall acts on the fd itself.
    let result = unsafe { libc::syscall(libc::SYS_ioctl, fd, UI_DEV_CREATE) };
    ioctl_errno(result)
}

/// Read the kernel-assigned `input<N>` name (`UI_GET_SYSNAME`).
///
/// # Safety contract
/// * Precondition: `fd` is a valid open `/dev/uinput` descriptor for an
///   already-created device.
/// * Postcondition: on `Ok`, returns the `input<N>` identifier; on `Err`, the
///   read failed. The output buffer is 256 bytes as required by the ABI.
pub(crate) fn ui_get_sysname(fd: RawFd) -> io::Result<Vec<u8>> {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is a valid 256-byte buffer; `UI_GET_SYSNAME` is `_IOR(...
    // char[256])`, so the kernel writes at most 256 bytes. The buffer lives
    // for the duration of the call; the written bytes are copied into the
    // returned `Vec` (NUL-trimmed) before `buf` is dropped.
    let result = unsafe {
        libc::syscall(
            libc::SYS_ioctl,
            fd,
            UI_GET_SYSNAME,
            buf.as_mut_ptr().cast::<c_void>(),
        )
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    let len = (result as usize).min(buf.len());
    let mut out = buf[..len].to_vec();
    if out.last() == Some(&0) {
        out.pop();
    }
    Ok(out)
}

/// Check that the ioctl request numbers match the kernel's definitions.
///
/// These are compile-time constants; the test locks the ABI numbers so a
/// toolchain or header change cannot silently alter the requests Ferrokey
/// sends to the kernel.
#[cfg(test)]
mod abi_tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_uapi_linux_uinput_h() {
        // Values derived from the stable kernel ABI:
        //   #define UI_DEV_CREATE _IO(UINPUT_IOCTL_BASE, 1)
        //   #define UI_DEV_SETUP  _IOW(UINPUT_IOCTL_BASE, 3, struct uinput_setup)
        //   #define UI_SET_EVBIT  _IOW(UINPUT_IOCTL_BASE, 100, int)
        //   #define UI_SET_KEYBIT _IOW(UINPUT_IOCTL_BASE, 101, int)
        //   #define UI_GET_SYSNAME _IOR(UINPUT_IOCTL_BASE, 300, char[256])
        // Note: the C _IOC macro ORs (dir<<30)|(size<<16)|(type<<8)|nr, and
        // 'U' (0x55) has bit 0 set, so nr=300's bit 8 (0x100) is absorbed
        // into the type byte — the resulting number is 0x8100552c, exactly
        // as the kernel computes it.
        assert_eq!(UI_DEV_CREATE, 0x5501);
        assert_eq!(UI_DEV_SETUP, 0x405c_5503);
        assert_eq!(UI_SET_EVBIT, 0x4004_5564);
        assert_eq!(UI_SET_KEYBIT, 0x4004_5565);
        assert_eq!(UI_GET_SYSNAME, 0x8100_552c);
    }

    #[test]
    fn uinput_setup_layout_matches_kernel() {
        assert_eq!(std::mem::size_of::<InputId>(), 8);
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(UINPUT_MAX_NAME_SIZE, 80);
    }
}
