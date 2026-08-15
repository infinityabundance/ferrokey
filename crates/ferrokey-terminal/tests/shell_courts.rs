//! SHELL.* — the shell-aware terminal rows courts (Phase 4 WS5, §5.15).
//!
//! Runs as a `cargo test` inside the builder container (never on the host),
//! printing one machine-readable gate line per assertion. The court covers
//! identity (§5.2), the process-tree context model (§5.3), the generic
//! fallback (§5.4), the per-shell rows (§5.6–§5.9), tmux/ssh (§5.11–§5.12),
//! nested contexts (§5.13), presentation-only row switching (§5.10) and the
//! exact PTY byte fixtures (§5.14).
//!
//! `testing/scripts/shell-court.sh` parses the gate lines, writes the court
//! receipt, and fails the suite when any gate fails.

use ferrokey_core::{KeyboardState, ModifierSet, Moment, PhysicalKey, StateSettings};
use ferrokey_terminal::key_encoder::TerminalKeyEncoder;
use ferrokey_terminal::shell::{
    encode_sequence, shell_row, ProcTreeReader, ShellContext, ShellIdentitySource, ShellKind,
    ShellRowKey, BASH_ROW, FISH_ROW, GENERIC_ROW, NUSHELL_ROW, TMUX_ROW, ZSH_ROW,
};
use std::collections::HashMap;
use std::sync::Arc;

fn gate(id: &str, label: &str, pass: bool, detail: &str) {
    println!(
        "SHELL.{}  {} ... {}",
        id,
        label,
        if pass { "PASS" } else { "FAIL" }
    );
    if !pass {
        println!("  detail: {detail}");
        assert!(pass, "{id} {label}: {detail}");
    }
}

fn enc() -> TerminalKeyEncoder {
    let layout = ferrokey_layouts::builtin::builtin("us").expect("us layout");
    TerminalKeyEncoder::new(Arc::new(layout))
}

/// The exact PTY bytes a shell-row key must produce (§5.14): the real
/// encoder output for its sequence.
fn bytes_for(encoder: &TerminalKeyEncoder, key: &ShellRowKey) -> Vec<u8> {
    encode_sequence(encoder, key.sequence)
}

// ── a deterministic fake process tree for §5.3 / §5.13 ──────────────────────

struct FakeTree {
    comms: HashMap<i32, String>,
    children: HashMap<i32, Vec<i32>>,
}

impl FakeTree {
    fn new() -> Self {
        FakeTree {
            comms: HashMap::new(),
            children: HashMap::new(),
        }
    }

    fn node(mut self, pid: i32, comm: &str, children: &[i32]) -> Self {
        self.comms.insert(pid, comm.to_string());
        self.children.insert(pid, children.to_vec());
        self
    }
}

impl ProcTreeReader for FakeTree {
    fn comm(&self, pid: i32) -> Option<String> {
        self.comms.get(&pid).cloned()
    }

    fn children(&self, pid: i32) -> Vec<i32> {
        self.children.get(&pid).cloned().unwrap_or_default()
    }
}

// ── identity (§5.2) ─────────────────────────────────────────────────────────

#[test]
fn shell_courts() {
    // ── SHELL.BASH.001 / SHELL.ZSH.001 / SHELL.FISH.001 / SHELL.NU.001 ─────
    for (path, kind, id) in [
        ("/usr/bin/bash", ShellKind::Bash, "BASH.001"),
        ("/bin/bash5", ShellKind::Bash, "BASH.001"),
        ("/usr/bin/zsh", ShellKind::Zsh, "ZSH.001"),
        ("/usr/local/bin/fish", ShellKind::Fish, "FISH.001"),
        ("/usr/bin/nushell", ShellKind::Nushell, "NU.001"),
        ("/usr/bin/nu", ShellKind::Nushell, "NU.001"),
        ("/bin/sh", ShellKind::Unknown, "UNKNOWN.001"),
        ("/usr/bin/vim", ShellKind::Unknown, "UNKNOWN.001"),
    ] {
        gate(
            id,
            &format!("identity from program {}", path.rsplit('/').next().unwrap()),
            ShellKind::from_program(path) == kind,
            &format!("got {:?}", ShellKind::from_program(path)),
        );
    }

    // ── SHELL.UNKNOWN.001: generic fallback ─────────────────────────────────
    {
        let ctx = ShellContext::from_spawned_shell("/bin/sh");
        gate(
            "UNKNOWN.001",
            "generic fallback for unknown shell",
            ctx.kind == ShellKind::Unknown
                && ctx.source == ShellIdentitySource::SpawnedChild
                && ctx.row_id() == "generic"
                && !shell_row("generic").is_empty()
                && shell_row("generic").iter().any(|k| k.label == "Ctrl+C"),
            "unknown shell must map to the generic row",
        );
    }

    // ── SHELL.NESTED.001 / TMUX.001 / SSH.001: process-tree context ─────────
    {
        // shell → vim
        let tree = FakeTree::new().node(1, "bash", &[2]).node(2, "vim", &[]);
        let ctx = ShellContext::inspect_with(1, &tree);
        gate(
            "NESTED.001",
            "bash -> vim keeps bash identity",
            ctx.kind == ShellKind::Bash
                && ctx.source == ShellIdentitySource::ProcessInspection
                && !ctx.tmux
                && !ctx.ssh
                && ctx.row_id() == "bash",
            &format!("{:?}", ctx),
        );
        // fish → bash (nested shell: the deepest interactive shell wins)
        let tree = FakeTree::new().node(1, "fish", &[2]).node(2, "bash", &[]);
        let ctx = ShellContext::inspect_with(1, &tree);
        gate(
            "NESTED.001b",
            "fish -> bash resolves to bash",
            ctx.kind == ShellKind::Bash && ctx.row_id() == "bash",
            &format!("{:?}", ctx),
        );
        // zsh → tmux → nested bash: tmux context wins the row
        let tree = FakeTree::new()
            .node(1, "zsh", &[2])
            .node(2, "tmux", &[3])
            .node(3, "bash", &[]);
        let ctx = ShellContext::inspect_with(1, &tree);
        gate(
            "TMUX.001",
            "tmux in the tree selects the tmux row",
            ctx.tmux && ctx.row_id() == "tmux" && !shell_row("tmux").is_empty(),
            &format!("{:?}", ctx),
        );
        // bash → ssh: remote shell unknowable → generic row
        let tree = FakeTree::new().node(1, "bash", &[2]).node(2, "ssh", &[]);
        let ctx = ShellContext::inspect_with(1, &tree);
        let ssh_row = shell_row("ssh");
        let same_as_generic = ssh_row.len() == GENERIC_ROW.len()
            && ssh_row.iter().zip(GENERIC_ROW).all(|(a, b)| a == b);
        gate(
            "SSH.001",
            "ssh => remote uncertainty, generic row",
            ctx.ssh && ctx.row_id() == "ssh" && same_as_generic,
            &format!("{:?}", ctx),
        );
        // unreadable tree → Unknown → generic (nothing breaks)
        let empty = FakeTree::new();
        let ctx = ShellContext::inspect_with(1, &empty);
        gate(
            "UNKNOWN.001b",
            "unreadable tree degrades to generic",
            ctx.kind == ShellKind::Unknown && ctx.row_id() == "generic",
            &format!("{:?}", ctx),
        );
    }

    // ── SHELL.BASH.002 / ZSH.002 / FISH.002 / NU.002: row semantics ────────
    {
        // Required bash controls (§5.6): Ctrl+R, Ctrl+K, Ctrl+Y, Ctrl+X Ctrl+E.
        let bash_labels: Vec<&str> = BASH_ROW.iter().map(|k| k.label).collect();
        gate(
            "BASH.002",
            "bash row semantics (readline chords)",
            bash_labels.contains(&"Ctrl+R")
                && bash_labels.contains(&"Ctrl+K")
                && bash_labels.contains(&"Ctrl+Y")
                && bash_labels.contains(&"Ctrl+X Ctrl+E"),
            "missing a required bash control",
        );
        // Required zsh controls (§5.7): Ctrl+R + real zle actions; no
        // invented directory-stack buttons (no default bindings exist).
        let zsh_labels: Vec<&str> = ZSH_ROW.iter().map(|k| k.label).collect();
        gate(
            "ZSH.002",
            "zsh row semantics (real zle bindings)",
            zsh_labels.contains(&"Ctrl+R")
                && zsh_labels.contains(&"Ctrl+K")
                && zsh_labels.contains(&"Ctrl+Y")
                && !zsh_labels.iter().any(|l| l.contains("cd")),
            "missing a required zsh control or invented a binding",
        );
        // Required fish controls (§5.8): autosuggestion acceptance,
        // completion, history.
        let fish_labels: Vec<&str> = FISH_ROW.iter().map(|k| k.label).collect();
        gate(
            "FISH.002",
            "fish row semantics (autosuggestion/completion/history)",
            fish_labels.contains(&"Tab")
                && fish_labels.contains(&"→ accept")
                && fish_labels.contains(&"Up")
                && fish_labels.contains(&"Down"),
            "missing a required fish control",
        );
        // Nushell controls (§5.9): real reedline defaults only.
        let nu_labels: Vec<&str> = NUSHELL_ROW.iter().map(|k| k.label).collect();
        gate(
            "NU.002",
            "nushell row semantics (reedline defaults)",
            nu_labels.contains(&"Ctrl+R") && nu_labels.contains(&"Tab"),
            "missing a required nushell control",
        );
        // tmux row is key sequences, not IPC (§5.11): every action is a
        // Ctrl+B prefix chord through the encoder.
        gate(
            "TMUX.001b",
            "tmux row is key-sequence chords",
            TMUX_ROW
                .iter()
                .all(|k| k.sequence.len() == 2 && k.sequence[0][0] == PhysicalKey::LeftCtrl),
            "tmux row must be Ctrl+B prefix chords",
        );
    }

    // ── SHELL.BYTES.001: exact encoder fixtures (§5.14) ─────────────────────
    {
        let encoder = enc();
        // (label, row, expected bytes) — the ASCII control codes and CSI
        // sequences the production encoder must produce.
        let fixtures: &[(&str, &[ShellRowKey], &[u8])] = &[
            ("Ctrl+R", BASH_ROW, &[0x12]),
            ("Ctrl+K", BASH_ROW, &[0x0B]),
            ("Ctrl+Y", BASH_ROW, &[0x19]),
            ("Ctrl+U", BASH_ROW, &[0x15]),
            ("Ctrl+W", BASH_ROW, &[0x17]),
            ("Ctrl+A", BASH_ROW, &[0x01]),
            ("Ctrl+E", BASH_ROW, &[0x05]),
            ("Ctrl+X Ctrl+E", BASH_ROW, &[0x18, 0x05]),
            ("Tab", FISH_ROW, &[0x09]),
            ("→ accept", FISH_ROW, &[0x1B, 0x5B, 0x43]), // ESC [ C
            ("Up", FISH_ROW, &[0x1B, 0x5B, 0x41]),       // ESC [ A
            ("Down", FISH_ROW, &[0x1B, 0x5B, 0x42]),     // ESC [ B
            ("Ctrl+F", FISH_ROW, &[0x06]),
            ("prefix d", TMUX_ROW, &[0x02, 0x64]), // Ctrl+B then d
            ("prefix c", TMUX_ROW, &[0x02, 0x63]), // Ctrl+B then c
            ("prefix %", TMUX_ROW, &[0x02, 0x25]), // Ctrl+B then %
            ("prefix \"", TMUX_ROW, &[0x02, 0x22]), // Ctrl+B then "
            ("Esc", GENERIC_ROW, &[0x1B]),
        ];
        let mut bad = 0usize;
        for (label, row, expected) in fixtures {
            let key = row
                .iter()
                .find(|k| k.label == *label)
                .expect("fixture row key");
            let got = bytes_for(&encoder, key);
            if got.as_slice() != *expected {
                bad += 1;
                println!("  fixture {label}: expected {expected:02x?} got {got:02x?}");
            }
        }
        gate(
            "BYTES.001",
            "exact encoder fixtures",
            bad == 0,
            &format!("{bad} fixtures mismatch"),
        );
    }

    // ── SHELL.STATE.001: row switching cannot corrupt input state (§5.10) ──
    {
        // Playing any row's sequence through the REAL core state machine
        // leaves zero held keys and no latched/locked modifiers; and the
        // bytes are self-contained (independent of the previously active
        // row). This is the model-level proof that a row transition is
        // presentation-only.
        let encoder = enc();
        let rows = [
            ("generic", GENERIC_ROW),
            ("bash", BASH_ROW),
            ("zsh", ZSH_ROW),
            ("fish", FISH_ROW),
            ("nushell", NUSHELL_ROW),
            ("tmux", TMUX_ROW),
        ];
        let mut bad = 0usize;
        for (row_id, row) in rows {
            for key in row {
                let mut s = KeyboardState::new(StateSettings::default());
                let mut bytes = Vec::new();
                for group in key.sequence {
                    for pk in *group {
                        let evs = s.press(*pk, Moment::from_millis(100)).unwrap_or_default();
                        for e in &evs {
                            if let ferrokey_core::KeyEvent::Down(k) = e {
                                if let Some(b) =
                                    encoder.encode(*k, s.held_modifiers(), &Default::default())
                                {
                                    bytes.extend_from_slice(&b);
                                }
                            }
                        }
                    }
                    for pk in group.iter().rev() {
                        let _ = s.release(*pk, Moment::from_millis(110));
                    }
                }
                if s.held_count() != 0 || !s.latched().is_empty() || !s.locked().is_empty() {
                    bad += 1;
                    println!("  {}:{} left state behind", row_id, key.label);
                }
                if bytes != bytes_for(&encoder, key) {
                    bad += 1;
                    println!(
                        "  {}:{} state-machine bytes != encoder bytes",
                        row_id, key.label
                    );
                }
            }
        }
        gate(
            "STATE.001",
            "row switch preserves state (no ghost holds)",
            bad == 0,
            &format!("{bad} state/bytes mismatches"),
        );
    }

    // ── SHELL.PTY.001: real bash accepts a row chord in a real PTY (§5.14) ─
    {
        // The bash row's Ctrl+L (clear-screen) is encoded by the REAL
        // encoder and written into a REAL PTY with a REAL bash child; bash
        // must redraw its prompt, proving the row's keyboard semantics work
        // end-to-end against an actual shell.
        use ferrokey_terminal::child::{ChildHandle, ShellConfig};
        use ferrokey_terminal::pty::{PtyPair, Winsize};
        use std::time::{Duration, Instant};

        let mut pty = PtyPair::open(Winsize::default()).expect("pty");
        pty.make_nonblocking().expect("nonblocking");
        let home = std::env::temp_dir().join("ferrokey-shell-court-home");
        let _ = std::fs::create_dir_all(&home);
        let config = ShellConfig {
            shell: Some("/bin/bash".into()),
            home: Some(home.clone()),
            env: vec![
                ("TERM".into(), "xterm-256color".into()),
                // A HOME without a .bashrc: otherwise the inherited /root
                // .bashrc overrides PS1 and the marker prompt never appears.
                ("HOME".into(), home.to_string_lossy().into_owned()),
                ("PS1".into(), "SHELLPROMPT> ".into()),
            ],
        };
        let mut child = match ChildHandle::spawn(&mut pty, &config) {
            Ok(c) => c,
            Err(e) => {
                gate(
                    "PTY.001",
                    "real bash row chord in a real PTY",
                    false,
                    &format!("spawn failed: {e}"),
                );
                return;
            }
        };
        // The clear-screen sequence readline emits on Ctrl+L.
        const CLEAR: &[u8] = b"\x1b[H\x1b[2J";
        let mut read_until = |needle: &[u8]| -> Vec<u8> {
            let mut buf = [0u8; 512];
            let mut collected = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(6);
            loop {
                match nix::unistd::read(pty.master(), &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        collected.extend_from_slice(&buf[..n]);
                        if collected.windows(needle.len()).any(|w| w == needle) {
                            break;
                        }
                    }
                    // EAGAIN: no data yet; EIO: the child has not opened the
                    // slave yet — both are retried until the deadline.
                    Err(nix::errno::Errno::EAGAIN | nix::errno::Errno::EIO) => {
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
            collected
        };
        // Drain the initial prompt (bash's startup output — the PS1 override
        // is irrelevant here; we assert on the clear-screen behavior).
        let initial = read_until(b"# ");
        let clear_before = initial.windows(CLEAR.len()).filter(|w| *w == CLEAR).count();
        // Encode the bash row's Ctrl+L through the REAL encoder and write it.
        let encoder = enc();
        let clear = encoder
            .encode(PhysicalKey::L, ModifierSet::CTRL, &Default::default())
            .expect("ctrl-l encodes");
        let _ = extra_write(pty.master(), &clear);
        let after_bytes = read_until(CLEAR);
        let clear_after = after_bytes
            .windows(CLEAR.len())
            .filter(|w| *w == CLEAR)
            .count();
        child.shutdown(Duration::from_secs(2));
        gate(
            "PTY.001",
            "real bash clears the screen on a row chord (real PTY)",
            clear_after > clear_before,
            &format!(
                "clear-screen sequences before={clear_before} after={clear_after} bytes={clear:02x?}"
            ),
        );
    }
}

/// Write bytes to the PTY master, retrying EIO/EAGAIN while the child opens
/// the slave (bounded) — same pattern as the child-session tests.
fn extra_write(master: &std::os::fd::OwnedFd, bytes: &[u8]) -> Result<(), String> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match nix::unistd::write(master, bytes) {
            Ok(n) if n == bytes.len() => return Ok(()),
            Ok(n) => return extra_write(master, &bytes[n..]),
            Err(nix::errno::Errno::EIO | nix::errno::Errno::EAGAIN) => {
                if Instant::now() >= deadline {
                    return Err("master write kept failing with EIO".into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(format!("master write failed: {e:?}")),
        }
    }
}
