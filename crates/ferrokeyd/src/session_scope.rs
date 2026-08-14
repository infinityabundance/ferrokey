//! Logind session-scope binding (§28, §99).
//!
//! The broker can be bound to a specific graphical login session: a client
//! is then authorized only if it lives in the *same* session scope. The
//! session of a process is not exposed by the kernel directly — but the
//! session manager places every session in a dedicated cgroup whose name is
//! the logind scope (`session-N.scope`), so a process's session scope is
//! readable from its cgroup path (`/proc/<pid>/cgroup`).
//!
//! # Runtime constraints
//!
//! The authorization happens AFTER the seccomp freeze, so this module must
//! not rely on any syscall outside the (widened) allowlist. The widening is
//! a single, narrowly-gated `openat`:
//!
//! ```text
//! openat(dirfd = the pre-opened /proc fd, path = "<pid>/cgroup",
//!        flags = O_RDONLY|O_CLOEXEC) → allowed
//! openat with any other dirfd, flags, or path shape → EPERM (§35, §60)
//! ```
//!
//! The seccomp gate (see `sandbox::SessionGate`) enforces the dirfd and the
//! flags at the syscall level; the code in this module additionally enforces
//! the path contract (a validated decimal pid, nothing else), so the only
//! file the broker is *expected* to open is a `cgroup` file of a well-formed
//! pid. Note the exact authority boundary: seccomp constrains the syscall
//! *arguments* (`dirfd`, `flags`) — it does not inspect the pathname memory
//! — so the enforced authority is read-only `openat` relative to the
//! pre-opened `/proc` directory under a highly constrained syscall shape,
//! and a compromised broker could use the gate to read other world-readable
//! `/proc/<pid>/…` files with `O_RDONLY`. Opening with `O_WRONLY`/`O_RDWR`
//! (e.g. `/dev/uinput`, block devices) stays impossible (§35), so injection
//! authority is unchanged.

use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, OwnedFd};

/// The `session-N.scope` component inside a cgroup path.
pub fn scope_from_cgroup(text: &str) -> Option<String> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // cgroup v2: "0::/sys/fs/cgroup/.../session-2.scope"
        // cgroup v1: "2:name=systemd:/user.slice/.../session-2.scope"
        let path = line.rsplit_once(':').map(|(_, p)| p).unwrap_or(line);
        let component = path.rsplit('/').next().unwrap_or("");
        if crate::config::is_valid_session_scope(component) {
            return Some(component.to_string());
        }
    }
    None
}

/// The broker's own session scope, resolved BEFORE the freeze
/// (`/proc/self/cgroup` is read with the full pre-freeze filesystem reach).
/// Returns `None` when the broker is not inside a logind session scope
/// (e.g. started from an SSH session or a plain service).
pub fn self_scope() -> Option<String> {
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    scope_from_cgroup(&text)
}

/// The post-freeze session lookup: a pre-opened `/proc` directory fd plus the
/// bound scope. The ONLY file the frozen broker can open is
/// `openat(proc_fd, "<pid>/cgroup", O_RDONLY|O_CLOEXEC)` (§28, §99).
pub struct SessionScopeGate {
    proc_dirfd: OwnedFd,
    bound_scope: String,
}

impl SessionScopeGate {
    /// Open the `/proc` directory fd and bind the scope.
    ///
    /// # Postconditions
    /// * `proc_dirfd` is a directory fd (O_PATH|O_DIRECTORY|O_CLOEXEC) that
    ///   the seccomp gate bakes into the runtime filter; the fd inventory
    ///   (security.rs §37) accounts for it.
    /// * The scope must be a valid `session-N.scope` name (config-validated
    ///   already, re-checked here because this is the syscall-adjacent
    ///   boundary).
    pub fn open(bound_scope: &str) -> std::io::Result<Self> {
        use nix::fcntl::{open, OFlag};
        if !crate::config::is_valid_session_scope(bound_scope) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid session scope {bound_scope:?}"),
            ));
        }
        let proc_dirfd = open(
            "/proc",
            OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )?;
        Ok(SessionScopeGate {
            proc_dirfd,
            bound_scope: bound_scope.to_string(),
        })
    }

    /// The raw `/proc` directory fd (for the seccomp gate + FD inventory).
    pub fn proc_dirfd(&self) -> i32 {
        self.proc_dirfd.as_raw_fd()
    }

    /// The bound scope this gate enforces.
    pub fn bound_scope(&self) -> &str {
        &self.bound_scope
    }

    /// Resolve the peer's session scope from its cgroup, post-freeze.
    ///
    /// # Contract
    /// * The only path ever opened is `"{pid}/cgroup"` where `pid` is a
    ///   validated, non-zero decimal integer — no separators, no `.`/`..`,
    ///   no magic-link traversal is reachable through this construction
    ///   (the seccomp gate additionally fixes `dirfd` and `flags`).
    /// * Returns `None` when the peer is not inside any logind session scope
    ///   (SSH/CI/service processes), when `/proc/<pid>/cgroup` is unreadable,
    ///   or on any error — the caller denies the connection (§99).
    pub fn peer_scope(&self, pid: u32) -> Option<String> {
        use nix::fcntl::{openat, OFlag};
        if pid == 0 {
            return None;
        }
        let path = format!("{pid}/cgroup");
        let file = openat(
            &self.proc_dirfd,
            path.as_str(),
            OFlag::O_RDONLY | OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .ok()?;
        let mut file = File::from(file);
        let mut text = String::new();
        file.read_to_string(&mut text).ok()?;
        scope_from_cgroup(&text)
    }

    /// Whether the peer's session scope matches the bound scope.
    pub fn peer_is_in_bound_session(&self, pid: u32) -> bool {
        match self.peer_scope(pid) {
            Some(scope) => scope == self.bound_scope,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cgroup_v2_paths() {
        assert_eq!(
            scope_from_cgroup("0::/sys/fs/cgroup/user.slice/user-1000.slice/session-2.scope"),
            Some("session-2.scope".into())
        );
        assert_eq!(
            scope_from_cgroup(
                "0::/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/app.slice/app-x.scope"
            ),
            None
        );
    }

    #[test]
    fn parses_cgroup_v1_paths() {
        // Debian 10-era hybrid hierarchies: `hierarchy:name:path`.
        assert_eq!(
            scope_from_cgroup("1:name=systemd:/user.slice/user-1000.slice/session-3.scope"),
            Some("session-3.scope".into())
        );
    }

    #[test]
    fn scans_all_lines_for_a_session_scope() {
        // Hybrid v1 systems list multiple controllers; any line may carry
        // the scope component.
        let text = "\
0::/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service
1:name=systemd:/user.slice/user-1000.slice/session-7.scope";
        assert_eq!(scope_from_cgroup(text), Some("session-7.scope".into()));
    }

    #[test]
    fn no_session_scope_when_not_in_one() {
        assert_eq!(scope_from_cgroup("0::/"), None);
        assert_eq!(scope_from_cgroup("0::/system.slice/sshd.service"), None);
        assert_eq!(scope_from_cgroup(""), None);
        assert_eq!(scope_from_cgroup("garbage without colon"), None);
    }

    #[test]
    fn gate_rejects_invalid_scopes() {
        assert!(SessionScopeGate::open("session-2.scope").is_ok());
        assert!(SessionScopeGate::open("../session-2.scope").is_err());
        assert!(SessionScopeGate::open("session-x.scope").is_err());
    }

    #[test]
    fn gate_resolves_self_scope_consistently() {
        // The gate must agree with the pre-freeze self_scope() for the
        // calling process (the test runs outside any logind session, so both
        // are None on CI hosts and both are the same scope inside one).
        let gate = SessionScopeGate::open("session-2.scope").expect("open /proc");
        let self_scope = self_scope();
        let via_gate = gate.peer_scope(std::process::id());
        assert_eq!(self_scope, via_gate);
    }

    #[test]
    fn zero_pid_is_denied() {
        let gate = SessionScopeGate::open("session-2.scope").expect("open /proc");
        assert!(!gate.peer_is_in_bound_session(0));
    }
}
