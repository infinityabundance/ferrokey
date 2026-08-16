//! Runtime security state: capabilities, `NO_NEW_PRIVS`, FD inventory, and
//! the "prove the sandbox" verification pass (§5, §6, §37, §57, §58, §59,
//! §105).
//!
//! # Contract (§41, §105, §106)
//!
//! Every security transition must succeed or the broker refuses to serve.
//! There is no "log a warning and continue insecurely" mode: the functions
//! in this module return errors and the caller aborts startup.
//!
//! # Unsafe discipline (§82)
//!
//! The only `unsafe` is [`capset`] (the `capget`/`capset` syscall pair) and
//! the prctl shim, isolated here with full documentation.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt as _;

/// Capability state of the process (full 64-bit sets; the kernel's V3
/// capability API stores caps 0-31 in one `cap_data` and caps 32+ in a
/// second).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapabilityState {
    pub effective: u64,
    pub permitted: u64,
    pub inheritable: u64,
}

impl CapabilityState {
    pub fn all_zero(&self) -> bool {
        self.effective == 0 && self.permitted == 0 && self.inheritable == 0
    }
}

impl fmt::Display for CapabilityState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "effective={:#x} permitted={:#x} inheritable={:#x}",
            self.effective, self.permitted, self.inheritable
        )
    }
}

const _LINUX_CAPABILITY_VERSION_3: u32 = 0x2008_0522;

/// Read the calling thread's capability sets (`capget`, V3).
pub fn capget() -> io::Result<CapabilityState> {
    // SAFETY: the header and data arrays are valid for the duration of the
    // syscall; the kernel only reads the header and writes the data. Layout
    // matches `__user_cap_header_struct`/`__user_cap_data_struct[2]`:
    //   header: { u32 version; int pid; }   (version, then pid = 0 → self)
    //   data[2]: each { u32 effective; u32 permitted; u32 inheritable; }
    let (result, effective, permitted, inheritable) = unsafe {
        let mut header: [u32; 2] = [_LINUX_CAPABILITY_VERSION_3, 0];
        // Two `cap_data` structs (caps 0-31 and 32+): 6 u32s = 24 bytes,
        // exactly what the kernel copies for V3.
        let mut data: [u32; 6] = [0; 6];
        let r = raw_syscall_capget(header.as_mut_ptr() as usize, data.as_mut_ptr() as usize);
        let effective = u64::from(data[0]) | (u64::from(data[3]) << 32);
        let permitted = u64::from(data[1]) | (u64::from(data[4]) << 32);
        let inheritable = u64::from(data[2]) | (u64::from(data[5]) << 32);
        (r, effective, permitted, inheritable)
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(CapabilityState {
        effective,
        permitted,
        inheritable,
    })
}

/// Drop every capability (effective, permitted, inheritable) — `capset` to
/// the empty set.
///
/// # Postconditions
/// * On `Ok`, the calling thread's capability sets are all zero (§5).
/// * A non-root process can always lower its own caps; this never *raises*
///   anything (capset cannot grant a capability the process lacks).
pub fn capset_empty() -> io::Result<()> {
    // SAFETY: as `capget`, but with all-zero data — the syscall only ever
    // lowers capabilities. The data array is valid for the duration.
    let result = unsafe {
        let mut header: [u32; 2] = [_LINUX_CAPABILITY_VERSION_3, 0];
        let mut data: [u32; 6] = [0; 6]; // two cap_data structs, all zero
        raw_syscall_capset(header.as_mut_ptr() as usize, data.as_mut_ptr() as usize)
    };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `capget(2)` — reads the calling thread's capability sets.
///
/// # Safety
/// * `header`/`data` must point to valid, correctly-typed buffers as
///   documented at the call site.
unsafe fn raw_syscall_capget(header: usize, data: usize) -> isize {
    unsafe { crate::sandbox::raw_syscall(SYS_CAPGET, &[header, data]) }
}

/// `capset(2)` — sets the calling thread's capability sets.
///
/// # Safety
/// * `header`/`data` must point to valid, correctly-typed buffers as
///   documented at the call site.
unsafe fn raw_syscall_capset(header: usize, data: usize) -> isize {
    unsafe { crate::sandbox::raw_syscall(SYS_CAPSET, &[header, data]) }
}

#[cfg(target_arch = "x86_64")]
const SYS_CAPGET: i64 = 125;
#[cfg(target_arch = "x86_64")]
const SYS_CAPSET: i64 = 126;
#[cfg(target_arch = "aarch64")]
const SYS_CAPGET: i64 = 90;
#[cfg(target_arch = "aarch64")]
const SYS_CAPSET: i64 = 91;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const SYS_CAPGET: i64 = -1;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const SYS_CAPSET: i64 = -1;

/// Set `PR_SET_NO_NEW_PRIVS` (§6). Fails if the kernel rejects it.
pub fn set_no_new_privs() -> io::Result<()> {
    nix::sys::prctl::set_no_new_privs().map_err(io::Error::from)
}

/// Whether `NO_NEW_PRIVS` is currently set (§59).
pub fn no_new_privs_active() -> io::Result<bool> {
    nix::sys::prctl::get_no_new_privs().map_err(io::Error::from)
}

/// Disable core dumps (`RLIMIT_CORE = 0`).
///
/// A core dump of the broker would be a plaintext capture of the keys it
/// processed — the one leak category specific to being a *keyboard* (the
/// data it guards is typed input, not credentials or tokens). Set both the
/// soft and hard limits so the process can never raise it again.
///
/// # Postconditions
/// * On `Ok`, both the soft and hard `RLIMIT_CORE` limits are 0 (see
///   [`core_dump_disabled`]); the change is permanent for the process.
pub fn set_core_dump_disabled() -> io::Result<()> {
    nix::sys::resource::setrlimit(nix::sys::resource::Resource::RLIMIT_CORE, 0, 0)
        .map_err(io::Error::from)
}

/// Whether core dumps are disabled: the soft `RLIMIT_CORE` limit is 0.
pub fn core_dump_disabled() -> io::Result<bool> {
    nix::sys::resource::getrlimit(nix::sys::resource::Resource::RLIMIT_CORE)
        .map(|(cur, _)| cur == 0)
        .map_err(io::Error::from)
}

/// Prevent the process from being traced or dumped (`PR_SET_DUMPABLE = 0`).
///
/// The broker is an unprivileged process (no setuid bit), so it is not
/// dumpable by default — but a keyboard must never rely on ambient defaults:
/// making non-dumpability explicit closes ptrace/`/proc/<pid>/mem` access
/// and core dumps even if a future deployment changes the file modes.
///
/// # Postconditions
/// * On `Ok`, `PR_GET_DUMPABLE` reports 0 (see [`non_dumpable`]).
pub fn set_non_dumpable() -> io::Result<()> {
    nix::sys::prctl::set_dumpable(false).map_err(io::Error::from)
}

/// Whether the process is non-dumpable (`PR_GET_DUMPABLE == 0`).
pub fn non_dumpable() -> io::Result<bool> {
    nix::sys::prctl::get_dumpable()
        .map(|d| !d)
        .map_err(io::Error::from)
}

/// The current effective uid.
pub fn euid() -> u32 {
    nix::unistd::geteuid().as_raw() as u32
}

/// The current effective gid.
pub fn egid() -> u32 {
    nix::unistd::getegid().as_raw() as u32
}

/// Refuse to serve when running as root (§7).
///
/// `allow_root` is the explicitly-named development/testing override: it
/// must never be enabled implicitly, and the broker prints a security
/// warning when it is used.
pub fn check_refuses_root(allow_root: bool) -> Result<(), SecurityError> {
    if euid() == 0 && !allow_root {
        return Err(SecurityError::RefusesRoot);
    }
    Ok(())
}

/// Run the pre-seccomp security checks and privilege hardening (§41, §5,
/// §6, §37, §57-59). Called by `serve` immediately before installing the
/// seccomp filter.
///
/// # Order
/// 1. refuse root unless the dev override is explicit (§7)
/// 2. zero capabilities and verify they stay zero (§5)
/// 3. set NO_NEW_PRIVS and verify (§6)
/// 4. disable core dumps (`RLIMIT_CORE = 0`) and verify
/// 5. make the process non-dumpable (`PR_SET_DUMPABLE = 0`) and verify
/// 6. FD inventory must match the expected baseline exactly (§37)
///
/// # Postconditions
/// * On `Ok`, the process is non-root, capability-free, NO_NEW_PRIVS is
///   active, core dumps are disabled and the process is non-dumpable; the FD
///   set exactly matches the inventory baseline. The caller then installs
///   seccomp (the last freeze step).
pub fn verify_before_freeze(
    allow_root: bool,
    expected_fds: &BTreeSet<i32>,
) -> Result<SecurityReport, SecurityError> {
    check_refuses_root(allow_root)?;

    // Capabilities: drop, then prove empty (§5, §58). Lowering your own
    // caps always succeeds, even unprivileged; this is idempotent.
    capset_empty().map_err(SecurityError::Capset)?;
    let caps = capget().map_err(SecurityError::Capability)?;
    if !caps.all_zero() {
        return Err(SecurityError::CapabilitiesNonZero(caps));
    }

    // NO_NEW_PRIVS: set, then prove (§6, §59). If the kernel refuses,
    // fail startup — never serve insecurely (§105, §106).
    set_no_new_privs().map_err(SecurityError::NoNewPrivs)?;
    if !no_new_privs_active().map_err(SecurityError::NoNewPrivs)? {
        return Err(SecurityError::NoNewPrivsNotSet);
    }

    // Core dumps: disable, then prove. A core dump is a plaintext capture
    // of every key the broker processed; if the kernel refuses, fail
    // startup — never serve with the leak surface open.
    set_core_dump_disabled().map_err(SecurityError::CoreDump)?;
    if !core_dump_disabled().map_err(SecurityError::CoreDump)? {
        return Err(SecurityError::CoreDumpNotDisabled);
    }

    // Dumpability: make non-dumpable explicit, then prove. A keyboard
    // must not be traceable or dumpable even if deployment modes change.
    set_non_dumpable().map_err(SecurityError::CoreDump)?;
    if !non_dumpable().map_err(SecurityError::CoreDump)? {
        return Err(SecurityError::CoreDumpNotDisabled);
    }

    // FD inventory: prove the process holds exactly the expected set. Once
    // seccomp is installed (next step) `open` is impossible, so the set
    // cannot change afterwards (§35, §37).
    let actual = fd_inventory();
    if actual != *expected_fds {
        return Err(SecurityError::UnexpectedFds {
            expected: expected_fds.iter().copied().collect(),
            actual: actual.iter().copied().collect(),
        });
    }

    Ok(SecurityReport {
        euid: euid(),
        egid: egid(),
        capabilities: caps,
        no_new_privs: true,
        core_dump_disabled: true,
        non_dumpable: true,
        seccomp: false, // installed by the caller next; proved by probes
        fds: actual,
    })
}

/// A read-only report of the broker's security state (§104).
///
/// The booleans are distinct *observed kernel states* (each set-and-proven
/// independently during the freeze); they are not flags driving behaviour,
/// so the state-machine refactor the lint suggests does not apply.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct SecurityReport {
    pub euid: u32,
    pub egid: u32,
    pub capabilities: CapabilityState,
    pub no_new_privs: bool,
    /// `RLIMIT_CORE` soft limit is 0 (core dumps disabled).
    pub core_dump_disabled: bool,
    /// `PR_GET_DUMPABLE` reports 0 (non-dumpable).
    pub non_dumpable: bool,
    pub seccomp: bool,
    pub fds: BTreeSet<i32>,
}

impl std::fmt::Display for SecurityReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "euid={} egid={} caps=[{}] no_new_privs={} core_dumps=off dumpable=no seccomp={} fds={:?}",
            self.euid,
            self.egid,
            self.capabilities,
            self.no_new_privs,
            self.seccomp,
            self.fds.iter().collect::<Vec<_>>()
        )
    }
}

/// Errors from the security freeze.
#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error("refusing to run as root (§7); use the explicitly-named --allow-root development override if you truly mean it")]
    RefusesRoot,
    #[error("cannot query capabilities: {0}")]
    Capability(io::Error),
    #[error("cannot zero capabilities: {0}")]
    Capset(io::Error),
    #[error("cannot set NO_NEW_PRIVS: {0}")]
    NoNewPrivs(io::Error),
    #[error("NO_NEW_PRIVS is not active: refusing to serve (§59)")]
    NoNewPrivsNotSet,
    #[error("cannot disable core dumps / make the process non-dumpable: {0}")]
    CoreDump(io::Error),
    #[error("core dumps are not disabled or the process is still dumpable: refusing to serve")]
    CoreDumpNotDisabled,
    #[error("seccomp enforcement could not be proven (§61, §92)")]
    SeccompNotProven,
    #[error("unexpected open file descriptors: expected {expected:?}, found {actual:?} (§37)")]
    UnexpectedFds {
        expected: Vec<i32>,
        actual: Vec<i32>,
    },
    #[error("capabilities are not zero: {0} (§5)")]
    CapabilitiesNonZero(CapabilityState),
}

/// Enumerate the process's open file descriptors via `/proc/self/fd` (§37).
///
/// The enumeration handle itself (the fd of the `/proc/self/fd` directory)
/// is excluded, so the inventory reflects only the process's real fds.
pub fn fd_inventory() -> BTreeSet<i32> {
    let mut fds = BTreeSet::new();
    if let Ok(mut dir) = nix::dir::Dir::open(
        "/proc/self/fd",
        nix::fcntl::OFlag::O_DIRECTORY | nix::fcntl::OFlag::O_RDONLY | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    ) {
        let dir_fd = dir.as_raw_fd();
        for entry in dir.iter().flatten() {
            let name = entry.file_name();
            if let Ok(name) = name.to_str() {
                if let Ok(fd) = name.parse::<i32>() {
                    if fd != dir_fd {
                        fds.insert(fd);
                    }
                }
            }
        }
    }
    fds
}

/// The expected runtime FD set: stdio + the given descriptors.
pub fn expected_fds(extra: &[i32]) -> BTreeSet<i32> {
    let mut set: BTreeSet<i32> = [0, 1, 2].into_iter().collect();
    for fd in extra {
        set.insert(*fd);
    }
    set
}

/// A helper used by tests and the security-status tool: the FD target of
/// `/proc/self/fd/N` (for diagnostics).
pub fn fd_target(fd: i32) -> String {
    std::fs::read_link(format!("/proc/self/fd/{fd}"))
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "?".into())
}

/// Drop supplementary groups, then setgid + setuid (used by the supervisor
/// to pre-drop the runtime identity before exec).
///
/// # Preconditions
/// * The caller must be privileged enough to setgroups/setgid/setuid
///   (typically root).
/// * The target uid must not be 0 (validated at config level).
///
/// # Postconditions
/// * On `Ok`, real/effective/saved uid and gid are all the target values and
///   the supplementary group list is empty.
pub fn drop_identity(uid: u32, gid: u32) -> io::Result<()> {
    drop_bounding_set()?;
    nix::unistd::setgroups(&[]).map_err(io::Error::from)?;
    nix::unistd::setgid(nix::unistd::Gid::from_raw(gid)).map_err(io::Error::from)?;
    nix::unistd::setuid(nix::unistd::Uid::from_raw(uid)).map_err(io::Error::from)?;
    Ok(())
}

/// Drop every capability from the process bounding set (`PR_CAPBSET_DROP`).
///
/// Must run **while still privileged**: `PR_CAPBSET_DROP` requires
/// `CAP_SETPCAP` in the effective set, which a non-root process no longer
/// has after `setuid`. Dropping the bounding set is permanent and inherited
/// by children, so the runtime broker executes with an empty bounding set
/// (§58: "constrain the bounding set as strongly as the architecture
/// permits").
///
/// # Preconditions
/// * The caller has `CAP_SETPCAP` (e.g. a root supervisor with it in the
///   bounding set).
///
/// # Postconditions
/// * On `Ok`, the process bounding set is empty. `PR_CAPBSET_DROP` of a cap
///   that is not present is a silent no-op, and cap numbers above the kernel's
///   `CAP_LAST_CAP` are rejected with `EINVAL` — capability numbers are dense
///   and ascending, so the first `EINVAL` means every remaining number is
///   also invalid and the loop stops there.
pub fn drop_bounding_set() -> io::Result<()> {
    for cap in 0..=63u32 {
        // SAFETY: `prctl(PR_CAPBSET_DROP, cap, 0, 0, 0)` with a valid cap
        // number; the syscall only ever removes from the bounding set. This
        // is the only prctl in this module beyond NO_NEW_PRIVS (§82: isolated
        // unsafe with documented pre/postconditions).
        let result = unsafe { nix::libc::prctl(nix::libc::PR_CAPBSET_DROP, cap, 0, 0, 0) };
        if result == -1 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(nix::libc::EINVAL) {
                // cap > CAP_LAST_CAP: the kernel has no such capability.
                break;
            }
            return Err(err);
        }
    }
    Ok(())
}

/// Attach a `pre_exec` closure to `cmd` that drops the runtime identity
/// (supplementary groups → gid → uid) inside the forked child, before `exec`.
///
/// # Postconditions
/// * On `Ok`-returning closure, the executed process runs as `uid`/`gid`
///   with no supplementary groups — it never executes as root (§3, §41).
/// * The closure performs only async-signal-safe syscalls; the `unsafe`
///   `pre_exec` call is isolated here (§82: credential manipulation).
pub fn command_with_dropped_identity(
    mut cmd: std::process::Command,
    uid: u32,
    gid: u32,
) -> std::process::Command {
    // SAFETY: `pre_exec` runs in the forked child between `fork` and `exec`;
    // only async-signal-safe credential syscalls are performed, and no
    // allocation occurs (nix takes a slice for setgroups). On failure the
    // exec is aborted and the child exits — the broker never starts with
    // the wrong identity.
    unsafe {
        cmd.pre_exec(move || {
            // Empty the bounding set FIRST (needs CAP_SETPCAP, which the
            // still-privileged child has), then drop groups/gid/uid: the
            // executed runtime has an empty bounding set and no privilege
            // (§58).
            drop_bounding_set()?;
            nix::unistd::setgroups(&[]).map_err(io::Error::from)?;
            nix::unistd::setgid(nix::unistd::Gid::from_raw(gid)).map_err(io::Error::from)?;
            nix::unistd::setuid(nix::unistd::Uid::from_raw(uid)).map_err(io::Error::from)?;
            Ok(())
        });
    }
    cmd
}

/// A tiny adapter so the FD inventory test can observe a live fd.
pub fn raw_fd_of<T: AsRawFd>(t: &T) -> i32 {
    t.as_raw_fd()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_state_zero_check() {
        assert!(CapabilityState::default().all_zero());
        let non_zero = CapabilityState {
            effective: 1,
            permitted: 0,
            inheritable: 0,
        };
        assert!(!non_zero.all_zero());
    }

    #[test]
    fn expected_fds_are_stdio_plus_extras() {
        let set = expected_fds(&[7, 9]);
        assert_eq!(set, BTreeSet::from([0, 1, 2, 7, 9]));
    }

    #[test]
    fn fd_inventory_contains_own_socketpair() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let fd_a = raw_fd_of(&a);
        let _ = b;
        let inv = fd_inventory();
        assert!(
            inv.contains(&fd_a),
            "inventory must list the open socket fd"
        );
    }

    #[test]
    fn capset_empty_is_idempotent_for_unprivileged() {
        // Lowering your own capabilities always succeeds, even unprivileged.
        let before = capget().unwrap();
        capset_empty().unwrap();
        let after = capget().unwrap();
        assert!(after.all_zero() || before == after);
    }

    #[test]
    fn core_dump_and_dumpability_hardening_is_provable() {
        // The hardening must be applied-and-provable in a test process, so a
        // future regression in the setter or the verifier is caught on
        // stable without a VM court.
        set_core_dump_disabled().unwrap();
        assert!(
            core_dump_disabled().unwrap(),
            "RLIMIT_CORE must read back as 0 after disabling"
        );
        set_non_dumpable().unwrap();
        assert!(
            non_dumpable().unwrap(),
            "PR_GET_DUMPABLE must read back as non-dumpable"
        );
    }

    #[test]
    fn kernel_rejects_out_of_range_capability_numbers() {
        // The kernel contract `drop_bounding_set` relies on: capability
        // numbers are dense and ascending, and `PR_CAPBSET_DROP` with a
        // number above `CAP_LAST_CAP` is rejected, never a silent success.
        // Without this, a loop over 0..=63 fails at cap 41 on kernels where
        // CAP_LAST_CAP is 40, killing the serve pre-exec. The rejection is
        // EINVAL when the caller has CAP_SETPCAP (the loop's documented
        // precondition) and EPERM otherwise — both are rejections; a 0
        // (success) is the failure mode this test guards against.
        let result = unsafe { nix::libc::prctl(nix::libc::PR_CAPBSET_DROP, 64, 0, 0, 0) };
        assert_eq!(result, -1, "out-of-range capability must be rejected");
        let errno = io::Error::last_os_error().raw_os_error();
        assert!(
            errno == Some(nix::libc::EINVAL) || errno == Some(nix::libc::EPERM),
            "rejection must be EINVAL (privileged) or EPERM (unprivileged), got {errno:?}"
        );
    }
}
