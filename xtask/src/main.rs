//! Ferrokey developer tooling (Phase 4 WS2).
//!
//! * `cargo xtask man` — render the troff man pages to a deterministic
//!   output directory and verify that the documented configuration examples
//!   parse through the REAL configuration parsers.
//!
//! The man pages are authored in standard troff (the native man-page format,
//! rendered by groff — already present on any system with man pages), so no
//! additional documentation toolchain is needed. Ordinary `cargo build`
//! never depends on groff: rendering is an explicit documentation/release
//! step.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    // xtask/Cargo.toml -> workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask manifest inside the workspace")
        .to_path_buf()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("man") => cmd_man(),
        Some(other) => Err(format!("unknown xtask command {other:?} (expected 'man')")),
        None => Err("usage: cargo xtask man".into()),
    };
    if let Err(e) = result {
        eprintln!("xtask: {e}");
        std::process::exit(1);
    }
}

/// Render every `docs/man/*.{1,5}` with groff into `docs/man/out/`, and
/// verify the config examples parse through the real parsers.
fn cmd_man() -> Result<(), String> {
    let root = repo_root();
    let man_dir = root.join("docs/man");
    let out_dir = man_dir.join("out");

    // groff must exist; the court fails rather than silently skipping.
    let groff_ok = Command::new("groff")
        .arg("-V")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !groff_ok {
        return Err(
            "groff not usable — install groff (groff-base on Debian/Ubuntu, groff on Arch)".into(),
        );
    }

    fs::create_dir_all(&out_dir).map_err(|e| format!("mkdir {out_dir:?}: {e}"))?;
    let mut pages = fs::read_dir(&man_dir)
        .map_err(|e| format!("read_dir {man_dir:?}: {e}"))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "1" || x == "5") && p.is_file())
        .collect::<Vec<_>>();
    pages.sort();

    let mut rendered = Vec::new();
    for page in &pages {
        let stem = page
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("bad page name {:?}", page))?;
        // `-man` macro set, ASCII text output, no form feeds: deterministic.
        let output = Command::new("groff")
            .arg("-man")
            .arg("-Tascii")
            .arg("-P-c")
            .arg(page)
            .output()
            .map_err(|e| format!("groff spawn for {stem}: {e}"))?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            return Err(format!("groff failed on {stem}:\n{err}"));
        }
        let out_path = out_dir.join(format!("{stem}.txt"));
        fs::write(&out_path, &output.stdout).map_err(|e| format!("write {out_path:?}: {e}"))?;
        println!(
            "rendered {stem} -> {} ({} bytes)",
            out_path.display(),
            output.stdout.len()
        );
        rendered.push(stem.to_string());
    }
    if rendered.is_empty() {
        return Err("no troff man pages found in docs/man/".into());
    }

    verify_examples(&man_dir)?;

    println!("man pages: {} rendered, examples verified", rendered.len());
    Ok(())
}

/// Extract the `.nf`/`.fi` example block after the `.SH EXAMPLE` heading and
/// parse it with the real configuration parser of the matching page.
fn verify_examples(man_dir: &Path) -> Result<(), String> {
    for (page, kind) in [
        ("ferrokey.yaml.5", ExampleKind::Ui),
        ("ferrokeyd.yaml.5", ExampleKind::Daemon),
    ] {
        let text =
            fs::read_to_string(man_dir.join(page)).map_err(|e| format!("read {page}: {e}"))?;
        let yaml =
            extract_example(&text).ok_or_else(|| format!("{page}: no .nf example block found"))?;
        match kind {
            ExampleKind::Ui => {
                ferrokey::config::UiConfig::parse(&yaml).map_err(|e| {
                    format!("{page}: example does not parse through UiConfig::parse: {e}")
                })?;
                println!("{page}: example parses (UiConfig)");
            }
            ExampleKind::Daemon => {
                ferrokeyd::config::DaemonConfig::parse(&yaml).map_err(|e| {
                    format!("{page}: example does not parse through DaemonConfig::parse: {e}")
                })?;
                println!("{page}: example parses (DaemonConfig)");
            }
        }
    }
    Ok(())
}

enum ExampleKind {
    Ui,
    Daemon,
}

/// troff convention: the example body sits between `.nf` and `.fi` after the
/// `.SH EXAMPLE` heading, indented by 4 spaces.
fn extract_example(text: &str) -> Option<String> {
    let mut in_section = false;
    let mut in_block = false;
    let mut lines_out: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !in_section {
            if trimmed == ".SH EXAMPLE" {
                in_section = true;
            }
            continue;
        }
        if trimmed == ".nf" {
            in_block = true;
            continue;
        }
        if trimmed == ".fi" {
            in_block = false;
            continue;
        }
        if trimmed.starts_with(".SH ") {
            break;
        }
        if in_block && line.starts_with("    ") {
            lines_out.push(line.trim_start_matches("    "));
        }
    }
    if lines_out.is_empty() {
        return None;
    }
    let mut yaml = lines_out.join("\n");
    yaml.push('\n');
    Some(yaml)
}
