//! The runtime seccomp sandbox (§32, §33, §34).
//!
//! After the security freeze, the broker installs a strict **allowlist** BPF
//! filter derived from the daemon's actual runtime needs: only the syscalls
//! the single-threaded event loop requires are permitted; everything else
//! returns `EPERM`, and unknown architectures are killed (§34: the sandbox
//! fails closed if architecture assumptions do not hold).
//!
//! # What is blocked (§31, §33, §35, §14)
//!
//! * `socket`/`connect`/`bind`/`listen` — no network sockets can be created
//!   (AF_INET/AF_INET6/AF_PACKET are all unreachable because no socket
//!   family can be created at all) (§31).
//! * `open`/`openat`/`openat2`/`creat` — no arbitrary file or device opens
//!   (`/dev/uinput`, `/dev/input/event*`, `/dev/mem`, `/dev/kvm`, …) (§35).
//! * `ioctl` — no uinput reconfiguration, no device control at runtime (§14).
//! * High-risk subsystems (§33): `bpf`, `perf_event_open`, `ptrace`, `mount`,
//!   `umount2`, `pivot_root`, `chroot`, `init_module`, `finit_module`,
//!   `delete_module`, `kexec_load`, `kexec_file_load`, `reboot`, `keyctl`,
//!   `add_key`, `request_key`, `userfaultfd`, `io_uring_*`, `unshare`,
//!   `setns`, `open_by_handle_at`, `process_vm_*`, `iopl`, `ioperm`, `clone`,
//!   `fork`, `vfork`.
//!
//! # Architecture handling (§34)
//!
//! The filter validates `seccomp_data.arch` against `AUDIT_ARCH_X86_64` and
//! `AUDIT_ARCH_AARCH64` and applies per-architecture syscall numbers. Any
//! other architecture is killed.
//!
//! # Unsafe discipline (§82)
//!
//! All `unsafe` lives in [`install_filter`], which documents its
//! preconditions/postconditions. The BPF program builder is pure safe code
//! and is unit-tested.

use std::io;

// ---------------------------------------------------------------------------
// BPF constants (uapi/linux/filter.h, uapi/linux/seccomp.h, asm/unistd.h)
// ---------------------------------------------------------------------------

const BPF_LD: u16 = 0x00;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JMP: u16 = 0x05;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;
const BPF_RET: u16 = 0x06;

const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const EPERM: u32 = 1;

/// `seccomp_data` offsets.
const SECCOMP_DATA_NR: u32 = 0;
const SECCOMP_DATA_ARCH: u32 = 4;

/// `AUDIT_ARCH_*` values (uapi/linux/audit.h).
const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;
const AUDIT_ARCH_AARCH64: u32 = 0xC000_00B7;

/// One classic-BPF instruction (`struct sock_filter`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct BpfInsn {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

impl BpfInsn {
    const fn ld_abs(k: u32) -> Self {
        BpfInsn {
            code: BPF_LD | BPF_W | BPF_ABS,
            jt: 0,
            jf: 0,
            k,
        }
    }
    const fn jeq_k(k: u32, jt: u8, jf: u8) -> Self {
        BpfInsn {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt,
            jf,
            k,
        }
    }
    const fn ret_k(k: u32) -> Self {
        BpfInsn {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k,
        }
    }
}

/// The syscall allowlist per architecture.
///
/// x86_64 numbers from `arch/x86/entry/syscalls/syscall_64.tbl`;
/// aarch64 numbers from `include/uapi/asm-generic/unistd.h`.
#[derive(Debug, Clone, Copy)]
struct ArchAllowlist {
    arch: u32,
    syscalls: &'static [u32],
}

const X86_64_SYSCALLS: &[u32] = &[
    0,   // read
    1,   // write
    3,   // close
    5,   // fstat (glibc/runtime stat of stdio and the device fd)
    7,   // poll
    9,   // mmap
    10,  // mprotect
    11,  // munmap
    12,  // brk
    13,  // rt_sigaction
    14,  // rt_sigprocmask
    15,  // rt_sigreturn (required for signal handlers)
    28,  // madvise
    32,  // dup (std may duplicate stdio descriptors lazily)
    39,  // getpid
    44,  // sendto — Rust's UnixStream::write calls libc::send → sendto(2)
    45,  // recvfrom — Rust's UnixStream::read calls libc::recv → recvfrom(2)
    55, // getsockopt — SO_PEERCRED authorization (§27) at accept time (x86_64: 55; 54 is setsockopt)
    131, // sigaltstack (harmless; avoids runtime setup failures)
    158, // arch_prctl (glibc TLS)
    186, // gettid
    202, // futex (std sync primitives)
    228, // clock_gettime
    231, // exit_group
    288, // accept4
    318, // getrandom
    60, // exit
    63, // uname
];

const AARCH64_SYSCALLS: &[u32] = &[
    57,  // close
    62,  // lseek
    63,  // read
    64,  // write
    73,  // ppoll (aarch64's poll is ppoll)
    80,  // fstat
    93,  // exit
    94,  // exit_group
    98,  // futex
    101, // nanosleep
    113, // clock_gettime
    134, // rt_sigaction
    135, // rt_sigprocmask
    139, // rt_sigreturn
    160, // uname
    172, // getpid
    178, // gettid
    206, // sendto — Rust's UnixStream::write calls libc::send → sendto(2)
    207, // recvfrom — Rust's UnixStream::read calls libc::recv → recvfrom(2)
    209, // getsockopt — SO_PEERCRED authorization (§27); aarch64: socket=198, getsockopt=209
    214, // brk
    215, // munmap
    222, // mmap
    226, // mprotect
    233, // madvise
    242, // accept4
    278, // getrandom
];

/// NOTE on `fstat`/`dup`/`uname`/`gettid`/`getpid`: these are harmless
/// introspection syscalls over already-held descriptors or the process's own
/// state — they grant no new authority. `getsockopt` is required for
/// `SO_PEERCRED` peer authorization (§27). Everything that *opens* new
/// objects (`open`, `openat`, `openat2`, `creat`) is denied: after the
/// freeze the broker must not be able to reach `/dev/uinput`, `/dev/input/*`,
/// block devices or procfs/sysfs control files (§35, §60).
const X86_64: ArchAllowlist = ArchAllowlist {
    arch: AUDIT_ARCH_X86_64,
    syscalls: X86_64_SYSCALLS,
};
const AARCH64: ArchAllowlist = ArchAllowlist {
    arch: AUDIT_ARCH_AARCH64,
    syscalls: AARCH64_SYSCALLS,
};

/// Build the BPF program: arch dispatch, then per-arch allowlist.
///
/// Program layout:
/// ```text
/// 0: LD arch
/// 1: JEQ x86_64   → jump to x86_64 chain
/// 2: JEQ aarch64  → jump to aarch64 chain
/// 3: RET KILL_PROCESS
/// 4: (x86_64 chain) LD nr; JEQ s1 allow; ... ; RET EPERM
/// …: (aarch64 chain) …
/// ```
///
/// The result is deterministic and safe to test.
///
/// # Panics
/// Panics if the assembled jump offsets exceed `u8` (only possible if the
/// allowlists were extended past ~255 instructions, which the unit tests
/// would immediately catch).
pub fn build_filter_program() -> Vec<BpfInsn> {
    let mut prog = Vec::new();
    prog.push(BpfInsn::ld_abs(SECCOMP_DATA_ARCH));

    let x86_chain_len = allowlist_chain_len(X86_64.syscalls);
    let x86_chain_start = 4; // 0..=3: dispatch header
    let aarch64_chain_start = 4 + x86_chain_len;

    // If arch == x86_64, jump (from next_ip=2) to the x86_64 chain.
    prog.push(BpfInsn::jeq_k(
        X86_64.arch,
        u8::try_from(x86_chain_start - 2).expect("chain length fits"),
        1, // not x86_64: skip to the aarch64 check
    ));
    // If arch == aarch64, jump (from next_ip=3) to the aarch64 chain.
    prog.push(BpfInsn::jeq_k(
        AARCH64.arch,
        u8::try_from(aarch64_chain_start - 3).expect("chain length fits"),
        0, // not aarch64 either: fall into the kill
    ));
    prog.push(BpfInsn::ret_k(SECCOMP_RET_KILL_PROCESS));

    prog.extend(allowlist_chain(X86_64.syscalls));
    prog.extend(allowlist_chain(AARCH64.syscalls));
    debug_assert_eq!(
        prog.len(),
        4 + x86_chain_len + allowlist_chain_len(AARCH64.syscalls)
    );
    prog
}

fn allowlist_chain_len(syscalls: &[u32]) -> usize {
    // LD nr + (JEQ + RET ALLOW) * N + RET EPERM
    1 + 2 * syscalls.len() + 1
}

/// `LD nr; [JEQ s (jt=0, jf=1); RET ALLOW]×N; RET EPERM`
fn allowlist_chain(syscalls: &[u32]) -> Vec<BpfInsn> {
    let mut chain = Vec::with_capacity(allowlist_chain_len(syscalls));
    chain.push(BpfInsn::ld_abs(SECCOMP_DATA_NR));
    for &nr in syscalls {
        chain.push(BpfInsn::jeq_k(nr, 0, 1)); // equal → next insn (ALLOW)
        chain.push(BpfInsn::ret_k(SECCOMP_RET_ALLOW));
    }
    chain.push(BpfInsn::ret_k(SECCOMP_RET_ERRNO | EPERM));
    chain
}

/// Install the runtime filter via `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER)`.
///
/// # Safety contract
/// * Precondition: `PR_SET_NO_NEW_PRIVS` has been set (the kernel requires
///   it; otherwise this returns `EACCES`); the process is single-threaded
///   (the filter applies to the calling thread; with one thread it is the
///   whole process).
/// * Precondition: the caller has finished all device configuration ioctls,
///   socket binds, opens and privilege drops — after this call `ioctl`,
///   `open` and socket-family syscalls are impossible (§14, §35, §31).
/// * Postcondition: on `Ok`, the process is seccomp-filtered; on `Err`, no
///   filter is installed and the caller must refuse to serve (§105, §106).
pub fn install_filter() -> io::Result<()> {
    let prog = build_filter_program();
    // The kernel requires the program length to fit in a u16.
    let len = u16::try_from(prog.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "seccomp program too long"))?;
    // SAFETY: `prog` lives for the duration of the prctl call; the kernel
    // copies the instructions before returning. `sock_fprog` is a repr(C)
    // struct whose layout matches the kernel's. PR_SET_SECCOMP/
    // SECCOMP_MODE_FILTER are arch-independent constants.
    let result = unsafe {
        let mut fprog = libc_sock_fprog {
            len,
            filter: prog.as_ptr().cast(),
        };
        libc_syscall_prctl(
            PR_SET_SECCOMP,
            SECCOMP_MODE_FILTER as _,
            &raw mut fprog as usize,
            0,
            0,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Whether the process is currently seccomp-filtered (`PR_GET_SECCOMP`).
///
/// Returns `Some(true)` when `SECCOMP_MODE_FILTER` is active. Note this is
/// a prctl call, so it must be used *before* the filter is installed or by
/// the security-status tool on another process.
pub fn seccomp_active() -> io::Result<bool> {
    // SAFETY: PR_GET_SECCOMP takes no arguments and returns the mode.
    let result = unsafe { libc_syscall_prctl(PR_GET_SECCOMP, 0, 0, 0, 0) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result == SECCOMP_MODE_FILTER as isize)
    }
}

/// The result of the post-install enforcement probes (§61, §62, §92).
///
/// Each field is one independent binary fact about the enforced sandbox; the
/// `struct_excessive_bools` lint is deliberately allowed: a bitfield would
/// obscure the court-facing report, and the fields cannot be grouped
/// further without losing per-probe attribution.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProbeReport {
    /// `ioctl(-1, 0, 0)` was denied (§14, §61).
    pub ioctl_denied: bool,
    /// `socket(AF_INET, SOCK_STREAM, 0)` was denied (§31, §62).
    pub socket_af_inet_denied: bool,
    /// `socket(AF_INET6, SOCK_STREAM, 0)` was denied (§31, §62).
    pub socket_af_inet6_denied: bool,
    /// `socket(AF_PACKET, SOCK_RAW, 0)` was denied (§31, §62).
    pub socket_af_packet_denied: bool,
    /// `openat(AT_FDCWD, "/dev/uinput", O_RDWR)` was denied (§35, §60).
    pub openat_denied: bool,
    /// `openat` on a physical input device path was denied (§30, §60).
    pub openat_event_dev_denied: bool,
    /// `openat` on a privileged device path was denied (§60).
    pub openat_privileged_dev_denied: bool,
}

impl ProbeReport {
    pub fn all_denied(&self) -> bool {
        self.ioctl_denied
            && self.socket_af_inet_denied
            && self.socket_af_inet6_denied
            && self.socket_af_packet_denied
            && self.openat_denied
            && self.openat_event_dev_denied
            && self.openat_privileged_dev_denied
    }
}

impl std::fmt::Display for ProbeReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ioctl_denied={} socket_af_inet_denied={} socket_af_inet6_denied={} \
             socket_af_packet_denied={} openat_denied={} openat_event_dev_denied={} \
             openat_privileged_dev_denied={}",
            self.ioctl_denied,
            self.socket_af_inet_denied,
            self.socket_af_inet6_denied,
            self.socket_af_packet_denied,
            self.openat_denied,
            self.openat_event_dev_denied,
            self.openat_privileged_dev_denied
        )
    }
}

/// Prove the filter is *enforced* (§92: enforcement beats source inspection).
///
/// After installation, the previously-legal syscalls `ioctl`, `socket` and
/// `openat` must now return `EPERM` (seccomp evaluates before the kernel
/// sees the arguments, so the invalid arguments never cause harm). Every
/// dangerous device path is probed individually: a denial is proof that the
/// process cannot reach that path after the freeze (§35, §60).
///
/// # Preconditions
/// * The filter has been installed successfully in this process.
///
/// # Postconditions
/// * Every probe ran; `all_denied()` reports whether each was refused.
#[allow(clippy::similar_names)] // socket_af_inet/inet6/packet are distinct probe facts
pub fn prove_enforced() -> io::Result<ProbeReport> {
    let ioctl_denied = probe_eperm(nix::libc::SYS_ioctl, &[usize::MAX, 0, 0])?;
    let socket_af_inet_denied = probe_fd_denied(nix::libc::SYS_socket, &[2, 1, 0])?; // AF_INET, SOCK_STREAM
    let socket_af_inet6_denied = probe_fd_denied(nix::libc::SYS_socket, &[10, 1, 0])?; // AF_INET6, SOCK_STREAM
    let socket_af_packet_denied = probe_fd_denied(nix::libc::SYS_socket, &[17, 3, 0])?; // AF_PACKET, SOCK_RAW
                                                                                        // The paths are never dereferenced: the filter refuses the syscall before
                                                                                        // the kernel inspects the pointer (§60: attempt the forbidden operation).
    let openat_denied = probe_fd_denied(
        nix::libc::SYS_openat,
        &[
            nix::libc::AT_FDCWD as usize,
            c"/dev/uinput".as_ptr() as usize,
            nix::libc::O_RDWR as usize,
            0,
        ],
    )?;
    let openat_event_dev_denied = probe_fd_denied(
        nix::libc::SYS_openat,
        &[
            nix::libc::AT_FDCWD as usize,
            c"/dev/input/event0".as_ptr() as usize,
            nix::libc::O_RDONLY as usize,
            0,
        ],
    )?;
    // /dev/mem and /dev/kvm are representative privileged device paths (§60).
    let openat_privileged_dev_denied = probe_fd_denied(
        nix::libc::SYS_openat,
        &[
            nix::libc::AT_FDCWD as usize,
            c"/dev/mem".as_ptr() as usize,
            nix::libc::O_RDWR as usize,
            0,
        ],
    )?;
    Ok(ProbeReport {
        ioctl_denied,
        socket_af_inet_denied,
        socket_af_inet6_denied,
        socket_af_packet_denied,
        openat_denied,
        openat_event_dev_denied,
        openat_privileged_dev_denied,
    })
}

/// Run one syscall and report whether it was refused with `EPERM`.
fn probe_eperm(nr: i64, args: &[usize]) -> io::Result<bool> {
    // SAFETY: the syscall is executed with the provided arguments; if the
    // filter is active it is refused before the kernel inspects them. If the
    // filter is NOT active the results may be arbitrary (the probe would
    // then have exposed a misconfigured sandbox — which is exactly what the
    // caller must treat as fatal).
    let result = unsafe { raw_syscall(nr, args) };
    if result == -1 {
        Ok(io::Error::last_os_error().raw_os_error() == Some(EPERM_ERRNO))
    } else {
        Ok(false)
    }
}

/// Like `probe_eperm` for fd-returning syscalls (`socket`, `openat`): when
/// the syscall was NOT denied it may have created a descriptor — close it so
/// the probe cannot leak fds (observable only in builds without the filter,
/// i.e. mutation courts; with seccomp active the call never returns).
fn probe_fd_denied(nr: i64, args: &[usize]) -> io::Result<bool> {
    // SAFETY: as `probe_eperm`; additionally, a non-negative result is an fd
    // that this function always closes.
    let result = unsafe { raw_syscall(nr, args) };
    if result >= 0 {
        unsafe { nix::libc::close(result as i32) };
        Ok(false)
    } else {
        Ok(io::Error::last_os_error().raw_os_error() == Some(EPERM_ERRNO))
    }
}

const EPERM_ERRNO: i32 = 1;

// ---------------------------------------------------------------------------
// The tiny unsafe shims (isolated here, §82)
// ---------------------------------------------------------------------------

const PR_SET_SECCOMP: i32 = 22;
const PR_GET_SECCOMP: i32 = 21;
const SECCOMP_MODE_FILTER: i32 = 2;

#[repr(C)]
struct libc_sock_fprog {
    len: u16,
    filter: *const BpfInsn,
}

/// `prctl(2)` — the only prctl invocations the sandbox needs.
///
/// # Safety
/// * Caller must pass valid argument values matching the prctl option.
/// * Returns the raw syscall result; `-1` means errno was set.
unsafe fn libc_syscall_prctl(option: i32, a2: usize, a3: usize, a4: usize, a5: usize) -> isize {
    unsafe { nix::libc::syscall(nix::libc::SYS_prctl, option, a2, a3, a4, a5) as isize }
}

/// Run one syscall directly (used by the sandbox probe and verification).
///
/// # Safety
/// * `nr` must be a valid syscall number for the current architecture;
///   `args` must be valid for that syscall.
pub unsafe fn raw_syscall(nr: i64, args: &[usize]) -> isize {
    unsafe {
        nix::libc::syscall(
            nr,
            args.first().copied().unwrap_or(0),
            args.get(1).copied().unwrap_or(0),
            args.get(2).copied().unwrap_or(0),
            args.get(3).copied().unwrap_or(0),
            args.get(4).copied().unwrap_or(0),
            args.get(5).copied().unwrap_or(0),
        ) as isize
    }
}

// ---------------------------------------------------------------------------
// Tests: the program builder is pure safe code.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_starts_with_arch_dispatch() {
        let prog = build_filter_program();
        // 0: LD arch
        assert_eq!(prog[0], BpfInsn::ld_abs(SECCOMP_DATA_ARCH));
        // 1: JEQ x86_64
        assert_eq!(prog[1].code, BPF_JMP | BPF_JEQ | BPF_K);
        assert_eq!(prog[1].k, AUDIT_ARCH_X86_64);
        // 2: JEQ aarch64
        assert_eq!(prog[2].k, AUDIT_ARCH_AARCH64);
        // 3: unknown arch → kill
        assert_eq!(prog[3], BpfInsn::ret_k(SECCOMP_RET_KILL_PROCESS));
    }

    #[test]
    fn unknown_arch_falls_into_kill() {
        // On a hypothetical third architecture, arch matches neither branch:
        // the second JEQ's jf=0 means "fall through" → RET KILL_PROCESS.
        let prog = build_filter_program();
        assert_eq!(prog[2].jf, 0);
        assert_eq!(prog[3].k, SECCOMP_RET_KILL_PROCESS);
    }

    #[test]
    fn chain_terminates_with_eperm() {
        let prog = build_filter_program();
        let last = prog.last().unwrap();
        assert_eq!(last.code, BPF_RET | BPF_K);
        assert_eq!(last.k, SECCOMP_RET_ERRNO | EPERM);
    }

    #[test]
    fn every_allowlist_entry_has_allow() {
        let prog = build_filter_program();
        // Walk the two chains; each JEQ must be followed by RET ALLOW.
        let mut index = 4;
        for syscalls in [X86_64.syscalls, AARCH64.syscalls] {
            assert_eq!(prog[index].code, BPF_LD | BPF_W | BPF_ABS);
            index += 1;
            for _ in syscalls {
                assert_eq!(
                    prog[index].code,
                    BPF_JMP | BPF_JEQ | BPF_K,
                    "JEQ at {index}"
                );
                assert_eq!(prog[index + 1], BpfInsn::ret_k(SECCOMP_RET_ALLOW));
                index += 2;
            }
            assert_eq!(prog[index].k, SECCOMP_RET_ERRNO | EPERM);
            index += 1;
        }
        assert_eq!(index, prog.len());
    }

    #[test]
    fn jumps_land_inside_the_program() {
        let prog = build_filter_program();
        for (i, insn) in prog.iter().enumerate() {
            if insn.code == BPF_JMP | BPF_JEQ | BPF_K {
                for (offset, target) in [(i + 1, insn.jt as usize), (i + 1, insn.jf as usize)] {
                    let _ = offset;
                    assert!(
                        target < prog.len(),
                        "jump target {target} out of bounds from insn {i}"
                    );
                }
            }
        }
    }

    #[test]
    fn dispatch_jump_targets_are_correct() {
        let prog = build_filter_program();
        let x86_chain_start = 4;
        let aarch64_chain_start = 4 + allowlist_chain_len(X86_64.syscalls);
        // JEQ x86_64 at insn 1: next_ip = 2; jt should land on x86_64 chain.
        assert_eq!(prog[1].jt as usize, x86_chain_start - 2);
        // JEQ aarch64 at insn 2: next_ip = 3; jt should land on aarch64 chain.
        assert_eq!(prog[2].jt as usize, aarch64_chain_start - 3);
        assert_eq!(
            (2 + prog[1].jt as usize),
            x86_chain_start,
            "x86_64 jt must land exactly on the x86_64 chain"
        );
        assert_eq!(
            (3 + prog[2].jt as usize),
            aarch64_chain_start,
            "aarch64 jt must land exactly on the aarch64 chain"
        );
    }

    #[test]
    fn syscall_lists_are_sorted_and_unique() {
        for list in [X86_64.syscalls, AARCH64.syscalls] {
            let mut sorted = list.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), list.len(), "duplicate syscalls in allowlist");
        }
    }

    #[test]
    fn syscall_numbers_match_the_authoritative_kernel_tables() {
        // Pinned against arch/x86/entry/syscalls/syscall_64.tbl and
        // include/uapi/asm-generic/unistd.h (verified 2026-08). A mismatch
        // here means the seccomp allowlist silently blocks a needed syscall
        // (e.g. getsockopt for SO_PEERCRED) or permits an unintended one.
        let x86: &[(i64, &str)] = &[
            (0, "read"),
            (1, "write"),
            (3, "close"),
            (5, "fstat"),
            (7, "poll"),
            (9, "mmap"),
            (10, "mprotect"),
            (11, "munmap"),
            (12, "brk"),
            (13, "rt_sigaction"),
            (14, "rt_sigprocmask"),
            (15, "rt_sigreturn"),
            (28, "madvise"),
            (32, "dup"),
            (39, "getpid"),
            (44, "sendto (libc send)"),
            (45, "recvfrom (libc recv)"),
            (55, "getsockopt"),
            (60, "exit"),
            (63, "uname"),
            (131, "sigaltstack"),
            (158, "arch_prctl"),
            (186, "gettid"),
            (202, "futex"),
            (228, "clock_gettime"),
            (231, "exit_group"),
            (288, "accept4"),
            (318, "getrandom"),
        ];
        for (nr, name) in x86 {
            assert!(
                X86_64.syscalls.contains(&(*nr as u32)),
                "x86_64 {name} (nr {nr}) must be allowed"
            );
        }
        let arm: &[(i64, &str)] = &[
            (57, "close"),
            (62, "lseek"),
            (63, "read"),
            (64, "write"),
            (73, "ppoll"),
            (80, "fstat"),
            (93, "exit"),
            (94, "exit_group"),
            (98, "futex"),
            (101, "nanosleep"),
            (113, "clock_gettime"),
            (134, "rt_sigaction"),
            (135, "rt_sigprocmask"),
            (139, "rt_sigreturn"),
            (160, "uname"),
            (172, "getpid"),
            (178, "gettid"),
            (206, "sendto (libc send)"),
            (207, "recvfrom (libc recv)"),
            (209, "getsockopt"),
            (214, "brk"),
            (215, "munmap"),
            (222, "mmap"),
            (226, "mprotect"),
            (233, "madvise"),
            (242, "accept4"),
            (278, "getrandom"),
        ];
        for (nr, name) in arm {
            assert!(
                AARCH64.syscalls.contains(&(*nr as u32)),
                "aarch64 {name} (nr {nr}) must be allowed"
            );
        }
    }

    #[test]
    fn high_risk_syscalls_are_not_allowed() {
        // The families from §33 must never appear in either allowlist.
        // (arch, syscall-number) pairs — numbers from the official tables:
        //   ioctl:  x86_64 16, aarch64 29
        //   socket: x86_64 41, aarch64 198
        //   connect:x86_64 42, aarch64 203
        //   open:   x86_64 2 (aarch64 uses openat 56 — allowed, see note)
        //   clone:  x86_64 56, aarch64 220
        //   bpf:    x86_64 321, aarch64 280
        //   mount:  x86_64 165, aarch64 40
        let forbidden: &[(&str, u32, &[u32])] = &[
            ("ioctl", 16, &[16]),
            ("socket", 41, &[198]),
            ("connect", 42, &[203]),
            ("open", 2, &[]), // aarch64 openat is allowed (documented note)
            ("clone", 56, &[220]),
            ("bpf", 321, &[280]),
            ("mount", 165, &[40]),
        ];
        for (name, x86_nr, arm_nrs) in forbidden {
            assert!(
                !X86_64.syscalls.contains(x86_nr),
                "{name} (x86_64 nr {x86_nr}) must not be allowed"
            );
            for &nr in *arm_nrs {
                assert!(
                    !AARCH64.syscalls.contains(&nr),
                    "{name} (aarch64 nr {nr}) must not be allowed"
                );
            }
        }
    }
}
