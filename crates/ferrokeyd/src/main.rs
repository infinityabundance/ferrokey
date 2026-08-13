//! `ferrokeyd` — the Ferrokey constrained broker.
//!
//! # Phase 3 architecture
//!
//! ```text
//! ferrokeyd start    supervisor: config → fork init → fork serve → reap
//! ferrokeyd init     bootstrap:  open/configure/create/verify device → transfer fd → exit
//! ferrokeyd serve    runtime:    adopt fd → bind → freeze (caps/NNP/seccomp) → serve IPC
//! ferrokeyd sandbox-probe       host-safe probe of the exact runtime seccomp filter
//! ferrokeyd security-status     read-only security state of a running broker (§104)
//! ```
//!
//! The process that parses hostile IPC (`serve`) is never root, holds zero
//! capabilities, runs with `NO_NEW_PRIVS` and a strict seccomp allowlist,
//! has no network authority, cannot open files or devices after the freeze,
//! and can only write validated `EV_KEY` events to the single pre-created
//! virtual keyboard (§1-§19, §41).

use anyhow::{Context, Result};
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::process::exit;

fn main() {
    if let Err(e) = run_cli() {
        eprintln!("ferrokeyd: {e:#}");
        exit(1);
    }
}

fn run_cli() -> Result<()> {
    env_logger_basic();

    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("--help" | "-h") => {
            print_help();
            Ok(())
        }
        Some("--version" | "-V") => {
            println!("ferrokeyd {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("start") => cmd_start(&args[1..]),
        Some("init") => cmd_init(&args[1..]),
        Some("serve") => cmd_serve(&args[1..]),
        Some("sandbox-probe") => cmd_sandbox_probe(&args[1..]),
        Some("security-status") => cmd_security_status(&args[1..]),
        Some(other) => anyhow::bail!("unknown subcommand {other:?}; try --help"),
    }
}

// ---------------------------------------------------------------------------
// start — the supervisor
// ---------------------------------------------------------------------------

fn cmd_start(args: &[String]) -> Result<()> {
    let mut config: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                config = Some(PathBuf::from(
                    args.get(i + 1).context("--config requires a path")?,
                ));
                i += 2;
            }
            "--foreground" => i += 1, // accepted for compatibility; the supervisor is always in the foreground
            other => anyhow::bail!("unknown start argument: {other}"),
        }
    }
    ferrokeyd::bootstrap::run(config).map_err(|e| anyhow::anyhow!("supervisor failed: {e}"))
}

// ---------------------------------------------------------------------------
// init — the bootstrap component
// ---------------------------------------------------------------------------

fn cmd_init(args: &[String]) -> Result<()> {
    let handoff_fd: RawFd = args
        .first()
        .context("usage: ferrokeyd init <handoff-fd> [--device-name NAME] [--max-held-keys N]")?
        .parse()
        .context("handoff fd must be an integer")?;
    let mut device_name = ferrokey_uinput::DEVICE_NAME.to_string();
    let mut max_held_keys = 16usize;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--device-name" => {
                device_name.clone_from(args.get(i + 1).context("--device-name requires a value")?);
                i += 2;
            }
            "--max-held-keys" => {
                max_held_keys = args
                    .get(i + 1)
                    .context("--max-held-keys requires a value")?
                    .parse()
                    .context("max_held_keys must be an integer")?;
                i += 2;
            }
            other => anyhow::bail!("unknown init argument: {other}"),
        }
    }
    ferrokeyd::init::run(handoff_fd, &device_name, max_held_keys)
        .map_err(|e| anyhow::anyhow!("bootstrap failed: {e}"))
}

// ---------------------------------------------------------------------------
// serve — the runtime broker
// ---------------------------------------------------------------------------

fn cmd_serve(args: &[String]) -> Result<()> {
    let params = parse_serve_args(args)?;

    // The explicitly-named development override (§7): must print a warning.
    if params.allow_root {
        eprintln!(
            "WARNING: --allow-root is a development/testing override. \
             Production ferrokeyd must never run as root (§7)."
        );
    }

    ferrokeyd::serve::install_signal_handlers()?;
    ferrokeyd::serve::run(params).map_err(|e| anyhow::anyhow!("runtime broker failed: {e}"))
}

fn parse_serve_args(args: &[String]) -> Result<ferrokeyd::serve::ServeArgs> {
    let mut params = ferrokeyd::serve::ServeArgs {
        handoff_fd: -1,
        socket_path: PathBuf::from("/run/ferrokeyd/ferrokeyd.sock"),
        socket_mode: 0o666,
        allowed_uids: Vec::new(),
        allowed_gids: Vec::new(),
        max_connections: 1,
        burst: 200,
        per_second: 200,
        max_held_keys: 16,
        device_name: ferrokey_uinput::DEVICE_NAME.into(),
        allow_root: false,
    };
    let mut i = 1; // args[0] is the handoff fd (first positional), parsed below
    while i < args.len() {
        match args[i].as_str() {
            "--socket" => {
                params.socket_path =
                    PathBuf::from(args.get(i + 1).context("--socket requires a path")?);
                i += 2;
            }
            "--socket-mode" => {
                params.socket_mode =
                    parse_octal(args.get(i + 1).context("--socket-mode requires a value")?)?;
                i += 2;
            }
            "--max-conn" => {
                params.max_connections = parse_usize(args.get(i + 1))?;
                i += 2;
            }
            "--max-held" => {
                params.max_held_keys = parse_usize(args.get(i + 1))?;
                i += 2;
            }
            "--burst" => {
                params.burst = parse_u32(args.get(i + 1))?;
                i += 2;
            }
            "--per-sec" => {
                params.per_second = parse_u32(args.get(i + 1))?;
                i += 2;
            }
            "--device-name" => {
                params
                    .device_name
                    .clone_from(args.get(i + 1).context("--device-name requires a value")?);
                i += 2;
            }
            "--uid" => {
                params.allowed_uids = parse_u32_list(args.get(i + 1))?;
                i += 2;
            }
            "--gid" => {
                params.allowed_gids = parse_u32_list(args.get(i + 1))?;
                i += 2;
            }
            "--allow-root" => {
                params.allow_root = true;
                i += 1;
            }
            other => anyhow::bail!("unknown serve argument: {other}"),
        }
    }
    // The first positional argument is the handoff fd.
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    params.handoff_fd = positional
        .first()
        .context("usage: ferrokeyd serve <handoff-fd> [options]")?
        .parse()
        .context("handoff fd must be an integer")?;
    Ok(params)
}

fn parse_octal(s: &str) -> Result<u32> {
    let v = u32::from_str_radix(s.trim_start_matches("0o"), 8)
        .with_context(|| format!("invalid octal mode {s:?}"))?;
    if v > 0o777 {
        anyhow::bail!("socket mode {v:#o} exceeds 0o777");
    }
    Ok(v)
}

fn parse_usize(s: Option<&String>) -> Result<usize> {
    let s = s.context("missing value")?;
    s.parse().with_context(|| format!("invalid integer {s:?}"))
}

fn parse_u32(s: Option<&String>) -> Result<u32> {
    let s = s.context("missing value")?;
    s.parse().with_context(|| format!("invalid integer {s:?}"))
}

fn parse_u32_list(s: Option<&String>) -> Result<Vec<u32>> {
    let s = s.context("missing value")?;
    s.split(',')
        .filter(|p| !p.is_empty())
        .map(|p| {
            p.parse::<u32>()
                .with_context(|| format!("invalid uid/gid {p:?}"))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// sandbox-probe — host-safe proof of the exact runtime filter
// ---------------------------------------------------------------------------

fn cmd_sandbox_probe(_args: &[String]) -> Result<()> {
    use ferrokeyd::sandbox;
    // The probe mirrors the runtime freeze sequence for the seccomp part:
    // NO_NEW_PRIVS (required before loading a filter), install, prove.
    ferrokeyd::security::set_no_new_privs()?;
    sandbox::install_filter()?;
    let report = sandbox::prove_enforced()?;
    println!("seccomp enforcement probe: {report}");
    if !report.all_denied() {
        anyhow::bail!("sandbox probe FAILED: not all forbidden syscalls were denied");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// security-status — read-only diagnostics of a running broker (§104)
// ---------------------------------------------------------------------------

fn cmd_security_status(args: &[String]) -> Result<()> {
    let mut pid: Option<i32> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pid" => {
                pid = Some(
                    args.get(i + 1)
                        .context("--pid requires a value")?
                        .parse()
                        .context("invalid pid")?,
                );
                i += 2;
            }
            other => anyhow::bail!("unknown security-status argument: {other}"),
        }
    }
    let pid = pid.context("usage: ferrokeyd security-status --pid <pid>")?;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .with_context(|| format!("cannot read /proc/{pid}/status"))?;
    println!("=== ferrokeyd security status (pid {pid}) ===");
    for key in [
        "Uid",
        "Gid",
        "CapInh",
        "CapPrm",
        "CapEff",
        "CapBnd",
        "CapAmb",
        "NoNewPrivs",
        "Seccomp",
    ] {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix(&format!("{key}:")) {
                println!("{key}:{rest}");
            }
        }
    }
    println!("fds: {}", describe_fds(pid));
    Ok(())
}

fn describe_fds(pid: i32) -> String {
    let mut names = Vec::new();
    if let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) {
        for entry in entries.flatten() {
            if let Ok(target) = std::fs::read_link(entry.path()) {
                names.push(format!(
                    "{}->{}",
                    entry.file_name().to_string_lossy(),
                    target.display()
                ));
            }
        }
    }
    names.join(" ")
}

// ---------------------------------------------------------------------------
// Logging
// ---------------------------------------------------------------------------

/// A tiny env-logger replacement: `RUST_LOG`-style filtering without the
/// dependency, keeping the daemon's dependency tree small.
fn env_logger_basic() {
    let level = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());
    let level = match level.to_ascii_lowercase().as_str() {
        "error" => log::LevelFilter::Error,
        "warn" => log::LevelFilter::Warn,
        "debug" => log::LevelFilter::Debug,
        "trace" => log::LevelFilter::Trace,
        _ => log::LevelFilter::Info,
    };
    let _ = log::set_boxed_logger(Box::new(Logger(level)));
    log::set_max_level(level);
}

struct Logger(log::LevelFilter);

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= self.0
    }
    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "{} [{:5}] {}",
                record.level(),
                record.level(),
                record.args()
            );
        }
    }
    fn flush(&self) {}
}

fn print_help() {
    println!(
        "ferrokeyd — the Ferrokey constrained broker (Phase 3)\n\
         \n\
         USAGE:\n\
         \x20 ferrokeyd start [--config <path>]\n\
         \x20 ferrokeyd init <handoff-fd> [--device-name <name>] [--max-held-keys <n>]\n\
         \x20 ferrokeyd serve <handoff-fd> [options]\n\
         \x20 ferrokeyd sandbox-probe\n\
         \x20 ferrokeyd security-status --pid <pid>\n\
         \n\
         SECURITY MODEL:\n\
         \x20 The runtime broker (serve) runs non-root with zero capabilities,\n\
         \x20 NO_NEW_PRIVS and a strict seccomp allowlist. It owns exactly one\n\
         \x20 pre-created virtual keyboard and can only emit validated EV_KEY\n\
         \x20 events. No network, no arbitrary opens, no runtime ioctl.\n\
         \n\
         CONFIG:\n\
         \x20 /etc/ferrokey/ferrokeyd.yaml  (must be root-owned, 0644)\n"
    );
}
