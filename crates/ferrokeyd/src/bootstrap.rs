//! The supervisor (`ferrokeyd start`) — coordinates the bootstrap and
//! runtime processes (§15, §16, §41).
//!
//! ```text
//! start (root, brief)
//!   │  socketpair (a, b)
//!   ├── fork + exec init  (root; a)
//!   │      create+verify device → SCM_RIGHTS over a → exit 0
//!   │  wait: init must succeed
//!   ├── fork + pre-exec drop (b): setgroups/setgid/setuid
//!   │      exec serve (b) — never root, zero caps, no_new_privs, seccomp
//!   │  wait: propagate serve's exit status
//!   └── exit
//! ```
//!
//! The supervisor itself parses the *security-boundary configuration* while
//! privileged; per §45 that config must be root-owned and not
//! group/world-writable.

use crate::config::{ConfigError, DaemonConfig, SessionScopeConfig};
use crate::fds;
use crate::security;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Errors from the supervisor.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("cannot resolve service identity: {0}")]
    Identity(String),
    #[error("cannot create the bootstrap handoff socketpair: {0}")]
    Socketpair(std::io::Error),
    #[error("cannot spawn the bootstrap component: {0}")]
    SpawnInit(std::io::Error),
    #[error("bootstrap component failed (exit {0}): the virtual keyboard was not created")]
    InitFailed(String),
    #[error("cannot spawn the runtime broker: {0}")]
    SpawnServe(std::io::Error),
    #[error("runtime broker exited with {0}")]
    ServeExited(String),
    #[error("runtime broker was terminated by signal {0}")]
    ServeSignaled(String),
}

/// Run the supervisor: load config, bootstrap the device, spawn the runtime.
pub fn run(config_path: Option<PathBuf>) -> Result<(), StartError> {
    let config = load_config(config_path)?;
    let (uid, gid) = resolve_identity(&config)?;

    log::info!("ferrokeyd supervisor starting: {}", describe(&config));

    // The private handoff channel (init end = a, serve end = b).
    let (init_end, serve_end) = fds::handoff_socketpair().map_err(StartError::Socketpair)?;
    let init_end_raw = fds::raw_fd(&init_end);
    let serve_end_raw = fds::raw_fd(&serve_end);

    // ── 1. Bootstrap: create + verify the device, transfer the fd ────────
    let mut init = Command::new("/proc/self/exe");
    init.arg("init")
        .arg(init_end_raw.to_string())
        .arg("--device-name")
        .arg(&config.device_name)
        .arg("--max-held-keys")
        .arg(config.max_held_keys.to_string())
        .stdin(Stdio::null());
    // The init child only needs its own end; close the serve end in it.
    let close_serve_end = fds::close_in_child(serve_end_raw);
    let mut init = fds::command_with_close(init, close_serve_end);
    let init_status = init
        .spawn()
        .map_err(StartError::SpawnInit)?
        .wait()
        .map_err(StartError::SpawnInit)?;
    if !init_status.success() {
        return Err(StartError::InitFailed(format!("{init_status}")));
    }
    log::info!("bootstrap complete: device created and transferred");
    // The parent no longer needs the init end.
    drop(init_end);

    // ── 2. Runtime: pre-drop identity, then exec serve ───────────────────
    let mut serve = Command::new("/proc/self/exe");
    serve
        .arg("serve")
        .arg(serve_end_raw.to_string())
        .arg("--socket")
        .arg(&config.socket_path)
        .arg("--socket-mode")
        .arg(format!("{:#o}", config.socket_mode))
        .arg("--max-conn")
        .arg(config.max_connections.to_string())
        .arg("--max-held")
        .arg(config.max_held_keys.to_string())
        .arg("--burst")
        .arg(config.rate.burst.to_string())
        .arg("--per-sec")
        .arg(config.rate.per_second.to_string())
        .arg("--device-name")
        .arg(&config.device_name)
        .arg("--uid")
        .arg(join_u32s(&config.allowed_uids))
        .arg("--gid")
        .arg(join_u32s(&config.allowed_gids));
    match &config.session_scope {
        SessionScopeConfig::Explicit(scope) => {
            serve.arg("--session-scope").arg(scope);
        }
        // Auto resolves inside the runtime broker (serve) from ITS OWN
        // /proc/self/cgroup, pre-freeze — the process that enforces the gate
        // proves its own session membership. Pass the directive through.
        SessionScopeConfig::Auto => {
            serve.arg("--session-scope").arg("auto");
        }
        SessionScopeConfig::None => {}
    }
    serve.stdin(Stdio::null());
    // §3, §41: the runtime must never start as root. `command_with_dropped_identity`
    // attaches a pre-exec closure (isolated unsafe, §82) that drops
    // supplementary groups, then gid, then uid — all before exec.
    let serve = security::command_with_dropped_identity(serve, uid, gid);

    let mut serve = serve;
    let mut child = serve.spawn().map_err(StartError::SpawnServe)?;
    // The parent no longer needs the serve end.
    drop(serve_end);

    // Forward SIGTERM/SIGINT to the runtime child: `kill <supervisor>` must
    // stop the whole broker cleanly (the courts and systemd rely on this).
    let status = wait_with_signal_forwarding(&mut child).map_err(StartError::SpawnServe)?;
    if status.success() {
        log::info!("runtime broker exited cleanly");
        Ok(())
    } else if let Some(code) = status.code() {
        Err(StartError::ServeExited(code.to_string()))
    } else {
        use std::os::unix::process::ExitStatusExt;
        Err(StartError::ServeSignaled(
            status
                .signal()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "unknown".into()),
        ))
    }
}

/// Load the config; when privileged, the file must be root-owned and not
/// group/world-writable (§45).
fn load_config(explicit: Option<PathBuf>) -> Result<DaemonConfig, StartError> {
    if let Some(path) = explicit {
        return DaemonConfig::load(&path).map_err(StartError::Config);
    }
    for candidate in DaemonConfig::default_paths() {
        if candidate.exists() {
            return DaemonConfig::load(&candidate).map_err(StartError::Config);
        }
    }
    Err(StartError::Config(ConfigError::Invalid(
        "no configuration found; the security-boundary config lives at \
         /etc/ferrokey/ferrokeyd.yaml (deny-by-default — you must configure \
         allowed_uids/allowed_gids)"
            .into(),
    )))
}

/// Resolve the configured service user/group to numeric ids (§3).
fn resolve_identity(config: &DaemonConfig) -> Result<(u32, u32), StartError> {
    use nix::unistd::User;
    let user = User::from_name(&config.service_user)
        .map_err(|e| StartError::Identity(format!("cannot look up user: {e}")))?
        .ok_or_else(|| {
            StartError::Identity(format!("user '{}' does not exist", config.service_user))
        })?;
    let gid = if config.service_group == config.service_user {
        user.gid.as_raw()
    } else {
        nix::unistd::Group::from_name(&config.service_group)
            .map_err(|e| StartError::Identity(format!("cannot look up group: {e}")))?
            .map(|g| g.gid.as_raw())
            .ok_or_else(|| {
                StartError::Identity(format!("group '{}' does not exist", config.service_group))
            })?
    };
    if user.uid.as_raw() == 0 {
        return Err(StartError::Identity(
            "service_user resolves to uid 0; the runtime broker must run \
             unprivileged (§3)"
                .into(),
        ));
    }
    if gid == 0 {
        return Err(StartError::Identity(
            "service_group resolves to gid 0; the runtime broker must run \
             unprivileged (§3)"
                .into(),
        ));
    }
    Ok((user.uid.as_raw(), gid))
}

fn join_u32s(values: &[u32]) -> String {
    values
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Wait for the runtime child, forwarding SIGTERM/SIGINT to it so a signal
/// to the supervisor cleanly stops the broker (§81: defined shutdown).
fn wait_with_signal_forwarding(
    child: &mut std::process::Child,
) -> std::io::Result<std::process::ExitStatus> {
    use std::sync::atomic::Ordering;
    if crate::signals::install().is_err() {
        // Signal forwarding is best-effort for the supervisor; without it,
        // SIGKILL on the supervisor would orphan the runtime briefly.
        log::warn!("supervisor: cannot install signal handlers");
    }
    loop {
        if crate::signals::STOP.load(Ordering::SeqCst) {
            log::info!("supervisor: stop requested; forwarding SIGTERM to runtime");
            let pid = nix::unistd::Pid::from_raw(child.id() as i32);
            let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM);
            return child.wait();
        }
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn describe(config: &DaemonConfig) -> String {
    format!(
        "socket={} uids={:?} gids={:?} rate={}/s burst={} max_conn={} max_held_keys={} \
         service={}:{}",
        config.socket_path.display(),
        config.allowed_uids,
        config.allowed_gids,
        config.rate.per_second,
        config.rate.burst,
        config.max_connections,
        config.max_held_keys,
        config.service_user,
        config.service_group
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_u32s_round_trip() {
        assert_eq!(join_u32s(&[]), "");
        assert_eq!(join_u32s(&[1000]), "1000");
        assert_eq!(join_u32s(&[1, 2, 3]), "1,2,3");
    }
}
