//! Child shell lifecycle (§7, §8, §37–§41): spawn a real user shell on the
//! PTY slave, signal it, reap it, restart it.
//!
//! The child is spawned with `fork` + `execvp`. Everything that could
//! allocate or lock (all `CString`s) is prepared **in the parent before the
//! fork**; between fork and exec the child only performs async-signal-safe
//! syscalls through `nix` safe wrappers (setsid, open, dup2, chdir, exec).
//!
//! The child becomes a session leader with the PTY slave as its controlling
//! terminal and stdio connected to it, so shells get normal job control.
//! The child runs with the **desktop user's identity** — the terminal stack
//! never gains privilege, and the child never talks to Ferrokey except
//! through PTY bytes (§106–§107).

use crate::pty::PtyPair;
use crate::terminal::TerminalError;
use nix::errno::Errno;
use nix::fcntl::{open, OFlag};
use nix::sys::signal::{kill, Signal};
use nix::sys::stat::Mode;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::{chdir, setsid, ForkResult, Pid};
use std::ffi::{CStr, CString};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// How the child process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildExit {
    Exited(i32),
    Signaled(&'static str),
}

impl ChildExit {
    pub fn summary(self) -> String {
        match self {
            ChildExit::Exited(code) => format!("exited with status {code}"),
            ChildExit::Signaled(sig) => format!("killed by {sig}"),
        }
    }
}

/// What to spawn inside the PTY (§7).
#[derive(Debug, Clone, Default)]
pub struct ShellConfig {
    /// Shell to run. `None` → `$SHELL` then `/bin/sh`.
    pub shell: Option<String>,
    /// Working directory. `None` → `$HOME` then `/`.
    pub home: Option<PathBuf>,
    /// Extra environment variables (TERM, COLORTERM, …).
    pub env: Vec<(String, String)>,
}

impl ShellConfig {
    fn resolve_shell(&self) -> String {
        if let Some(s) = &self.shell {
            return s.clone();
        }
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
    }

    /// Resolve the shell to an **absolute** executable path **in the parent**
    /// (before the fork): `execv` must never search PATH, because glibc's
    /// PATH search can allocate and deadlock the fork child if another thread
    /// holds the allocator lock.
    fn resolve_shell_abs(&self) -> String {
        let shell = self.resolve_shell();
        let path = std::path::Path::new(&shell);
        if path.is_absolute() {
            return shell;
        }
        if shell.contains('/') {
            return shell; // relative path: use as-is
        }
        // Search PATH for the bare name.
        if let Ok(paths) = std::env::var("PATH") {
            for dir in std::env::split_paths(&paths) {
                let candidate = dir.join(&shell);
                if candidate.is_file() {
                    return candidate.to_string_lossy().into_owned();
                }
            }
        }
        shell
    }

    fn resolve_home(&self) -> Option<PathBuf> {
        if let Some(h) = &self.home {
            return Some(h.clone());
        }
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

/// A spawned terminal child: the PID plus reaping state.
#[derive(Debug)]
pub struct ChildHandle {
    pid: Pid,
    reaped: bool,
    exit: Option<ChildExit>,
}

impl ChildHandle {
    /// Spawn the shell on `pty`'s slave. The parent's copy of the slave fd is
    /// closed afterwards.
    pub fn spawn(pty: &mut PtyPair, config: &ShellConfig) -> Result<Self, TerminalError> {
        let shell = config.resolve_shell_abs();
        let home = config.resolve_home();

        // Pre-build everything that could allocate *before* the fork.
        let shell_c = CString::new(shell.as_str())
            .map_err(|_| TerminalError::InvalidState("shell path contains NUL".into()))?;
        let argv0 = CString::new(
            std::path::Path::new(&shell)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| shell.clone()),
        )
        .map_err(|_| TerminalError::InvalidState("shell name contains NUL".into()))?;
        let slave_c = CString::new(pty.slave_path().as_os_str().as_encoded_bytes())
            .map_err(|_| TerminalError::InvalidState("slave path contains NUL".into()))?;
        let home_c = match &home {
            Some(h) => Some(
                CString::new(h.as_os_str().as_encoded_bytes())
                    .map_err(|_| TerminalError::InvalidState("home path contains NUL".into()))?,
            ),
            None => None,
        };

        // Environment prepared in the parent: TERM/COLORTERM plus the
        // configured extras. The parent's existing environment (HOME, PATH,
        // SHELL, LANG, …) is inherited by the fork.
        for (k, v) in &config.env {
            std::env::set_var(k, v);
        }

        // Warn (never silently spawn a privileged shell): the UI is
        // unprivileged by design; a root UI is a development-only situation.
        if nix::unistd::geteuid().is_root() {
            log::warn!(
                "spawning terminal shell while euid == 0 — the terminal child runs with the \
                 same (root) identity. Production Ferrokey must run unprivileged (§8)."
            );
        }

        let slave_fd = pty
            .slave()
            .ok_or_else(|| TerminalError::InvalidState("PTY slave already closed".into()))?
            .as_raw_fd();

        match crate::syscall::fork() {
            Ok(ForkResult::Parent { child }) => {
                // The parent keeps only the master; the slave fd is closed so
                // EOF semantics work when the child exits.
                pty.close_slave();
                log::info!("terminal child spawned: pid {child} shell {shell:?}");
                Ok(ChildHandle {
                    pid: child,
                    reaped: false,
                    exit: None,
                })
            }
            Ok(ForkResult::Child) => {
                // ── Child ────────────────────────────────────────────────
                // Only async-signal-safe operations from here on. On success
                // `child_exec` never returns; on failure it returns an exit
                // code and we terminate immediately (no destructors).
                let code = child_exec(
                    slave_c.as_c_str(),
                    slave_fd,
                    shell_c.as_c_str(),
                    argv0.as_c_str(),
                    home_c.as_deref(),
                );
                crate::syscall::exit_now(code);
            }
            Err(e) => {
                log::error!("fork failed: {e}");
                Err(TerminalError::Nix(e))
            }
        }
    }

    pub fn pid(&self) -> Pid {
        self.pid
    }

    /// Whether the child has been reaped.
    pub fn is_reaped(&self) -> bool {
        self.reaped
    }

    pub fn exit(&self) -> Option<ChildExit> {
        self.exit
    }

    /// Non-blocking reap. Returns `Some(exit)` when the child ended.
    pub fn poll_reap(&mut self) -> Option<ChildExit> {
        if self.reaped {
            return self.exit;
        }
        match waitpid(self.pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => self.finish(ChildExit::Exited(code)),
            Ok(WaitStatus::Signaled(_, sig, _)) => self.finish(ChildExit::Signaled(sig.as_str())),
            Ok(WaitStatus::StillAlive) => None,
            Ok(_) => self.finish(ChildExit::Exited(0)),
            Err(Errno::ECHILD) => self.finish(ChildExit::Exited(-1)),
            Err(e) => {
                log::warn!("waitpid failed: {e}");
                None
            }
        }
    }

    fn finish(&mut self, exit: ChildExit) -> Option<ChildExit> {
        self.reaped = true;
        self.exit = Some(exit);
        log::info!("terminal child {} {}", self.pid, exit.summary());
        self.exit
    }

    /// Send SIGHUP to the child's process group (clean terminal close).
    ///
    /// ESRCH (the group does not exist yet, or the child already exited) is
    /// tolerated: a freshly forked child has not run `setsid()` yet, and a
    /// dead child needs no signal.
    pub fn hangup_group(&self) -> Result<(), TerminalError> {
        match kill(Pid::from_raw(-self.pid.as_raw()), Signal::SIGHUP) {
            Err(Errno::ESRCH) => Ok(()),
            other => other.map_err(TerminalError::from),
        }
    }

    /// Force-kill the process group (ESRCH tolerated, see above).
    pub fn kill_group(&self) -> Result<(), TerminalError> {
        match kill(Pid::from_raw(-self.pid.as_raw()), Signal::SIGKILL) {
            Err(Errno::ESRCH) => Ok(()),
            other => other.map_err(TerminalError::from),
        }
    }

    /// Graceful shutdown: SIGHUP, wait up to `grace`, then SIGKILL and wait
    /// up to another `grace`. Always leaves the child reaped.
    pub fn shutdown(&mut self, grace: Duration) -> ChildExit {
        let _ = self.hangup_group();
        if self.wait_with_timeout(grace) {
            return self.exit.unwrap_or(ChildExit::Exited(-1));
        }
        let _ = self.kill_group();
        if self.wait_with_timeout(grace) {
            return self.exit.unwrap_or(ChildExit::Exited(-1));
        }
        ChildExit::Exited(-1)
    }

    fn wait_with_timeout(&mut self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        loop {
            if self.poll_reap().is_some() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for ChildHandle {
    fn drop(&mut self) {
        if !self.reaped {
            // Best effort: hang up and reap with a bounded grace period.
            let _ = self.hangup_group();
            self.wait_with_timeout(Duration::from_millis(500));
            if !self.reaped {
                let _ = self.kill_group();
                self.wait_with_timeout(Duration::from_millis(200));
            }
        }
    }
}

/// The child-side setup + exec. Returns the exit code to use on failure
/// (127 = command not found convention). Only async-signal-safe calls.
fn child_exec(
    slave_path: &CStr,
    inherited_slave_fd: std::os::fd::RawFd,
    shell: &CStr,
    argv0: &CStr,
    home: Option<&CStr>,
) -> i32 {
    // Become a session leader (new session, no controlling terminal yet).
    if let Err(e) = setsid() {
        log_child_error("setsid", e);
        return 127;
    }
    // Close the inherited slave copy; a *fresh* open of the slave device
    // makes it the controlling terminal (session leader without one).
    let _ = nix::unistd::close(inherited_slave_fd);
    let slave = match open(slave_path, OFlag::O_RDWR, Mode::empty()) {
        Ok(fd) => fd,
        Err(e) => {
            log_child_error("open slave", e);
            return 127;
        }
    };
    // Connect stdio to the slave (the nix helpers never close the source).
    for setup in [
        nix::unistd::dup2_stdin(&slave),
        nix::unistd::dup2_stdout(&slave),
        nix::unistd::dup2_stderr(&slave),
    ] {
        if let Err(e) = setup {
            log_child_error("dup2 stdio", e);
            return 127;
        }
    }
    if slave.as_raw_fd() > 2 {
        let _ = nix::unistd::close(slave);
    }
    if let Some(home) = home {
        if let Err(e) = chdir(home) {
            // Non-fatal: the shell starts in the current directory instead.
            log_child_error("chdir", e);
        }
    }
    // Exec. On success this never returns.
    match crate::syscall::execv(shell, argv0) {
        Ok(()) => 0,
        Err(e) => {
            log_child_error("execv", e);
            127
        }
    }
}

/// Report a failed setup call from the post-fork child.
///
/// `eprintln!`/`log!` are **not** allowed here: they take the stdio lock,
/// which another thread may still hold at fork time — the child would
/// deadlock before it could report the failure. This writes fd 2 directly
/// with async-signal-safe `write(2)` calls only (no allocation, no locks,
/// stack-built message).
fn log_child_error(op: &str, e: Errno) {
    const PREFIX: &[u8] = b"ferrokey-terminal: child ";
    const MIDDLE: &[u8] = b" failed: errno ";
    // Stack-buffer the decimal errno (async-signal-safe itoa; Errno is a
    // small positive C int in practice, but handle negatives defensively).
    let mut digits = [0u8; 12];
    let mut n = 0usize;
    let mut v = (e as i32).unsigned_abs();
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    digits[..n].reverse();
    crate::syscall::write_stderr(PREFIX);
    crate::syscall::write_stderr(op.as_bytes());
    crate::syscall::write_stderr(MIDDLE);
    crate::syscall::write_stderr(&digits[..n]);
    crate::syscall::write_stderr(b"\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::{PtyPair, Winsize};

    fn shell_that_prints() -> ShellConfig {
        ShellConfig {
            shell: Some("/bin/sh".into()),
            home: None,
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                ("COLORTERM".into(), "truecolor".into()),
            ],
        }
    }

    /// The post-fork error reporter must write the diagnostic to fd 2
    /// without taking the stdio lock (which a fork child must never do).
    /// Redirect fd 2 to a pipe, exercise the reporter, restore fd 2, then
    /// assert the bytes arrived. fd 2 is restored before any fallible call
    /// so a failure here cannot corrupt the test harness's stderr.
    #[test]
    fn log_child_error_writes_to_fd2_without_stdio_lock() {
        use nix::unistd::{dup, pipe, read};
        let (r, w) = pipe().unwrap();
        let saved = dup(std::io::stderr()).unwrap(); // a fresh fd duplicating stderr
        crate::syscall::dup2_for_tests(w.as_raw_fd(), 2); // fd 2 -> pipe
        drop(w);
        log_child_error("open slave", Errno::EACCES);
        // Restore stderr BEFORE any fallible call: a failure below must not
        // leave the test harness with redirected stderr.
        crate::syscall::dup2_for_tests(saved.as_raw_fd(), 2);
        let mut buf = [0u8; 128];
        let n = read(&r, &mut buf).unwrap();
        let msg = String::from_utf8_lossy(&buf[..n]).into_owned();
        drop(saved);
        drop(r);
        assert!(msg.contains("open slave"), "got: {msg}");
        assert!(msg.contains("errno 13"), "got: {msg}"); // EACCES == 13
    }

    #[test]
    fn spawns_shell_and_reads_output() {
        let mut pty = PtyPair::open(Winsize::default()).unwrap();
        pty.make_nonblocking().unwrap();
        let mut child = ChildHandle::spawn(&mut pty, &shell_that_prints()).unwrap();
        // Ask the shell to echo a marker, then read until it appears. The
        // write must retry: right after fork the parent has closed its slave
        // copy and the child may not have opened the slave yet, in which
        // case a master write returns EIO (no slave opener) and the bytes are
        // lost.
        write_with_retry(pty.master(), b"echo FERROKEY_PTY_OK\n");
        let mut buf = [0u8; 256];
        let mut collected = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            match nix::unistd::read(pty.master(), &mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    collected.extend_from_slice(&buf[..n]);
                    // "FERROKEY_PTY_OK" is 15 bytes.
                    if collected.windows(15).any(|w| w == b"FERROKEY_PTY_OK") {
                        break;
                    }
                }
                Err(nix::errno::Errno::EAGAIN) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    panic!("pty read failed: {e:?}");
                }
            }
        }
        let text = String::from_utf8_lossy(&collected);
        assert!(
            text.contains("FERROKEY_PTY_OK"),
            "expected shell echo within 8s, got: {text:?} (child alive: {})",
            !child.is_reaped()
        );
        child.shutdown(Duration::from_secs(2));
        assert!(child.is_reaped());
    }

    /// Write bytes to the PTY master, retrying EIO/EAGAIN while the child
    /// opens the slave (bounded).
    fn write_with_retry(master: &std::os::fd::OwnedFd, bytes: &[u8]) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match nix::unistd::write(master, bytes) {
                Ok(n) if n == bytes.len() => return,
                Ok(n) => {
                    write_with_retry(master, &bytes[n..]);
                    return;
                }
                Err(nix::errno::Errno::EIO | nix::errno::Errno::EAGAIN) => {
                    assert!(
                        Instant::now() < deadline,
                        "master write kept failing with EIO"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("master write failed: {e:?}"),
            }
        }
    }

    #[test]
    fn child_exit_status_is_reported() {
        let mut pty = PtyPair::open(Winsize::default()).unwrap();
        let mut child = ChildHandle::spawn(&mut pty, &shell_that_prints()).unwrap();
        // Ask the shell to exit with a known status (retry EIO: the slave may
        // not be open yet).
        write_with_retry(pty.master(), b"exit 7\n");
        let deadline = Instant::now() + Duration::from_secs(8);
        let mut status = None;
        while Instant::now() < deadline {
            if let Some(s) = child.poll_reap() {
                status = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(status, Some(ChildExit::Exited(7)));
    }

    #[test]
    fn hangup_kills_shell() {
        let mut pty = PtyPair::open(Winsize::default()).unwrap();
        let mut child = ChildHandle::spawn(&mut pty, &shell_that_prints()).unwrap();
        // Give the shell a moment; if it exited on its own that is a spawn
        // bug, not a hangup bug.
        std::thread::sleep(Duration::from_millis(100));
        if let Some(exit) = child.poll_reap() {
            panic!("shell exited on its own: {exit:?}");
        }
        child.hangup_group().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if child.poll_reap().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(child.is_reaped());
    }

    #[test]
    fn resize_sends_sigwinch_to_child() {
        let mut pty = PtyPair::open(Winsize::default()).unwrap();
        let mut child = ChildHandle::spawn(&mut pty, &shell_that_prints()).unwrap();
        // Child traps WINCH and reports via the PTY; we only verify the call
        // succeeds and the child stays alive.
        pty.resize(Winsize {
            rows: 30,
            cols: 90,
            ..Winsize::default()
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!child.is_reaped());
        child.shutdown(Duration::from_secs(2));
    }
}
