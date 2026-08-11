//! `ferrokeyd` — the Ferrokey privileged broker.
//!
//! Usage:
//! ```text
//! ferrokeyd [--config <path>] [--socket <path>] [--foreground]
//! ```
//!
//! The daemon must run with access to `/dev/uinput` (e.g. via a systemd unit
//! with the appropriate capability or a dedicated group). It drops no
//! privileges itself: it is a *privileged* component by design — but its only
//! authority is `KEY_DOWN`/`KEY_UP`/`RELEASE_ALL` for the explicit capability
//! set, enforced by the kernel's uinput + the ledger.

use anyhow::{Context, Result};
use ferrokeyd::config::DaemonConfig;
use ferrokeyd::{Server, ServerError};
use std::path::PathBuf;
use std::sync::Arc;

fn main() -> Result<()> {
    // Minimal logging: timestamps + level to stderr.
    env_logger_basic();

    let args: Vec<String> = std::env::args().collect();
    let mut config_path: Option<PathBuf> = None;
    let mut socket_override: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => {
                i += 1;
                config_path = Some(PathBuf::from(
                    args.get(i).context("--config requires a path")?,
                ));
            }
            "--socket" => {
                i += 1;
                socket_override = Some(PathBuf::from(
                    args.get(i).context("--socket requires a path")?,
                ));
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => anyhow::bail!("unknown argument: {other}"),
        }
        i += 1;
    }

    let config = load_config(config_path)?;
    let config = match socket_override {
        Some(path) => DaemonConfig {
            socket_path: path,
            ..config
        },
        None => config,
    };
    config.validate()?;
    log::info!("ferrokeyd starting with {}", describe(&config));

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    Server::install_signal_handlers(&stop)?;

    let mut server = Server::bind(config)?;
    server.run().map_err(|e| match e {
        ServerError::Bind { path, source } => anyhow::anyhow!("cannot bind {path}: {source}"),
        other => anyhow::anyhow!("{other}"),
    })?;
    Ok(())
}

fn load_config(explicit: Option<PathBuf>) -> Result<DaemonConfig> {
    if let Some(path) = explicit {
        return DaemonConfig::load(&path)
            .with_context(|| format!("loading config {}", path.display()));
    }
    for candidate in DaemonConfig::default_paths() {
        if candidate.exists() {
            return DaemonConfig::load(&candidate)
                .with_context(|| format!("loading config {}", candidate.display()));
        }
    }
    // No config file: use defaults, which are deny-by-default and therefore
    // require the operator to provide one via --config anyway (validation
    // fails otherwise — that is intentional).
    Ok(DaemonConfig::default())
}

fn describe(config: &DaemonConfig) -> String {
    format!(
        "socket={} uids={:?} gids={:?} rate={}/s burst={} max_conn={} max_held_keys={}",
        config.socket_path.display(),
        config.allowed_uids,
        config.allowed_gids,
        config.rate.per_second,
        config.rate.burst,
        config.max_connections,
        config.max_held_keys
    )
}

fn print_help() {
    println!(
        "ferrokeyd — Ferrokey privileged broker\n\
         \n\
         USAGE:\n\
         \x20 ferrokeyd [OPTIONS]\n\
         \n\
         OPTIONS:\n\
         \x20 --config <path>   YAML config file\n\
         \x20 --socket <path>   override the socket path\n\
         \x20 -h, --help        print this help\n\
         \n\
         The daemon owns /dev/uinput. Configure allowed_uids/allowed_gids\n\
         (deny-by-default). Example config:\n\
         \n\
         \x20 socket_path: /run/user/1000/ferrokeyd.sock\n\
         \x20 allowed_uids: [1000]\n\
         \x20 rate: {{ burst: 200, per_second: 200 }}\n"
    );
}

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
    simple_logger(level);
}

fn simple_logger(level: log::LevelFilter) {
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
    let _ = log::set_boxed_logger(Box::new(Logger(level)));
    log::set_max_level(level);
}
