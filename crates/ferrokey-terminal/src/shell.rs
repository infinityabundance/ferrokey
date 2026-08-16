//! Shell-aware terminal key rows (Phase 4 WS5).
//!
//! The embedded terminal gives Ferrokey direct ownership of the terminal
//! interaction path — so the OSK can present **better terminal controls**
//! when the interactive shell is known. This module owns the *model*:
//!
//! * shell identity ([`ShellKind`]) and how it was learned
//!   ([`ShellIdentitySource`]) — the initial child is **known from the
//!   spawn** (§5.2), later transitions are learned from the process tree
//!   the terminal legitimately owns (§5.3);
//! * the per-shell **key rows** as pure keyboard semantics: every action is
//!   a key/chord sequence that flows through the normal
//!   [`crate::TerminalKeyEncoder`] into the PTY — never a hidden shell
//!   command (§5.5, §5.11);
//! * the generic fallback: `Unknown` shell ⇒ the generic terminal row;
//!   nothing else changes (§5.4).
//!
//! Rows are **presentation-only**: switching rows never releases held keys,
//! presses keys, changes modifier state, resets terminal modes, resizes the
//! terminal or restarts the child (§5.10).

use crate::key_encoder::TerminalKeyEncoder;
use std::path::Path;

/// The **default** rendered width factor of every shell-row key (the shortcut
/// row that replaces the terminal view's static row 1 when a shell is
/// detected). One source of truth for both the UI rendering
/// (`set_terminal_shortcut_row`) and the pointer bridge's hit-testing of the
/// rendered row — the two must agree or clicking a shell button would
/// hit-test against the static row's geometry and play the wrong chord.
///
/// Individual keys may override this with a wider factor when their label
/// would not fit at the default width (see [`ShellRowKey::width`]).
pub const SHELL_KEY_WIDTH: f32 = 1.25;

// ── shell identity ──────────────────────────────────────────────────────────

/// The identified interactive shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    Bash,
    Zsh,
    Fish,
    Nushell,
    Unknown,
}

impl ShellKind {
    /// Identify a shell from the executable the terminal spawned (the
    /// basename decides; versioned names like `bash5` still match by
    /// prefix). `sh` is deliberately `Unknown`: POSIX sh is not bash, and
    /// claiming a bash row for it would be dishonest (§5.12's
    /// correctness-over-cleverness rule applies here too).
    pub fn from_program(path: &str) -> ShellKind {
        let base = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        let base = base.trim_start_matches(|c: char| !c.is_ascii_alphabetic());
        if base.starts_with("bash") {
            ShellKind::Bash
        } else if base.starts_with("zsh") {
            ShellKind::Zsh
        } else if base.starts_with("fish") {
            ShellKind::Fish
        } else if base.starts_with("nu") {
            ShellKind::Nushell
        } else {
            ShellKind::Unknown
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            ShellKind::Bash => "bash",
            ShellKind::Zsh => "zsh",
            ShellKind::Fish => "fish",
            ShellKind::Nushell => "nushell",
            ShellKind::Unknown => "unknown",
        }
    }

    /// The row id for this kind (the generic row for `Unknown`).
    pub fn row_id(self) -> &'static str {
        match self {
            ShellKind::Bash => "bash",
            ShellKind::Zsh => "zsh",
            ShellKind::Fish => "fish",
            ShellKind::Nushell => "nushell",
            ShellKind::Unknown => "generic",
        }
    }
}

/// How a shell identity was established. Detection sources are **not**
/// equally authoritative (§5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellIdentitySource {
    /// Ferrokey chose the executable and spawned it: authoritative.
    SpawnedChild,
    /// Learned from the local process tree Ferrokey owns.
    ProcessInspection,
    /// Terminal-output heuristics (currently unused — weak evidence must
    /// never pretend to be authoritative when it is not).
    TerminalEvidence,
    /// No identity established.
    Unknown,
}

/// The shell-context model: which shell is in control of the terminal, how
/// we know, and whether tmux/ssh wrap it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellContext {
    pub kind: ShellKind,
    pub source: ShellIdentitySource,
    /// A tmux server is part of the local process tree: its row exposes
    /// key-sequence (prefix) controls, never tmux IPC (§5.11).
    pub tmux: bool,
    /// An ssh client is part of the local process tree: the remote shell is
    /// unknowable, so the generic remote-terminal row is used (§5.12).
    pub ssh: bool,
}

impl ShellContext {
    pub const UNKNOWN: ShellContext = ShellContext {
        kind: ShellKind::Unknown,
        source: ShellIdentitySource::Unknown,
        tmux: false,
        ssh: false,
    };

    /// The initial identity: the shell Ferrokey itself spawned (§5.2).
    pub fn from_spawned_shell(shell: &str) -> ShellContext {
        ShellContext {
            kind: ShellKind::from_program(shell),
            source: ShellIdentitySource::SpawnedChild,
            tmux: false,
            ssh: false,
        }
    }

    /// The active row id: ssh ⇒ generic remote row, tmux ⇒ tmux row,
    /// otherwise the shell's row (generic for unknown).
    pub fn row_id(self) -> &'static str {
        if self.ssh {
            "ssh"
        } else if self.tmux {
            "tmux"
        } else {
            self.kind.row_id()
        }
    }

    /// Whether the context is strong enough to offer a shell-specific row
    /// (spawned identity or process evidence; never terminal-output
    /// heuristics).
    pub fn is_confident(self) -> bool {
        matches!(
            self.source,
            ShellIdentitySource::SpawnedChild | ShellIdentitySource::ProcessInspection
        )
    }
}

// ── process-tree inspection (§5.3) ──────────────────────────────────────────

/// Reads the local process tree (production implementation reads `/proc`).
/// Factored as a trait so the courts can exercise nested transitions with a
/// deterministic fake tree.
pub trait ProcTreeReader {
    /// The command name (`/proc/<pid>/stat` comm) of `pid`, if readable.
    fn comm(&self, pid: i32) -> Option<String>;
    /// The child pids of `pid`, if readable.
    fn children(&self, pid: i32) -> Vec<i32>;
}

/// The production `/proc` reader.
pub struct ProcReader;

impl ProcTreeReader for ProcReader {
    fn comm(&self, pid: i32) -> Option<String> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // comm is the parenthesised field; it may itself contain spaces.
        let open = stat.find('(')?;
        let close = stat.rfind(')')?;
        if close <= open {
            return None;
        }
        Some(stat[open + 1..close].to_string())
    }

    fn children(&self, pid: i32) -> Vec<i32> {
        std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children"))
            .ok()
            .map(|s| {
                s.split_whitespace()
                    .filter_map(|p| p.parse::<i32>().ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl ShellContext {
    /// Inspect the local process tree below `pid` to learn the interactive
    /// context (§5.3). Deterministic bounded walk (depth ≤ 8): known shells
    /// anywhere in the tree establish the identity; tmux/ssh are tracked as
    /// wrappers. A failure to read any part of the tree degrades to
    /// `Unknown` (the generic fallback — nothing breaks).
    pub fn inspect(pid: i32) -> ShellContext {
        Self::inspect_with(pid, &ProcReader)
    }

    /// [`ShellContext::inspect`] over a pluggable reader (the courts use a
    /// deterministic fake tree).
    pub fn inspect_with(pid: i32, reader: &dyn ProcTreeReader) -> ShellContext {
        let mut kind = ShellKind::Unknown;
        let mut tmux = false;
        let mut ssh = false;
        let mut stack = vec![pid];
        let mut depth = 0usize;
        while let Some(p) = stack.pop() {
            depth += 1;
            if depth > 8 * 8 {
                break; // bounded walk
            }
            if let Some(comm) = reader.comm(p) {
                match comm.trim() {
                    "bash" | "zsh" | "fish" | "nu" | "nushell" => {
                        kind = ShellKind::from_program(comm.trim());
                    }
                    "tmux" => tmux = true,
                    "ssh" => ssh = true,
                    _ => {}
                }
            }
            let mut kids = reader.children(p);
            stack.append(&mut kids);
        }
        // The identity is authoritative only when we actually saw a shell.
        let source = if kind == ShellKind::Unknown {
            ShellIdentitySource::Unknown
        } else {
            ShellIdentitySource::ProcessInspection
        };
        ShellContext {
            kind,
            source,
            tmux,
            ssh,
        }
    }
}

// ── shell key rows (pure keyboard semantics) ────────────────────────────────

/// One shell-row action: a label, its rendered width factor and the **key
/// sequence** it plays.
///
/// The sequence is a list of press-groups: each group is pressed and fully
/// released before the next group starts. This is what makes tmux prefix
/// sequences (and the `%`/`"` after a prefix) expressible with honest
/// keyboard semantics: `[[Ctrl, B], [Shift, D5]]` presses Ctrl+B, releases
/// it, then presses Shift+5. Simple chords are single groups
/// (`[[Ctrl, C]]`) — identical to the view-level chord mechanism.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShellRowKey {
    /// The button label (e.g. "Ctrl+R").
    pub label: &'static str,
    /// Rendered width factor (relative to the view's base key width). Most
    /// keys use [`SHELL_KEY_WIDTH`]; a key whose label does not fit at that
    /// width carries an explicit wider factor so the label is never cropped.
    pub width: f32,
    /// The key sequence, as press-groups of physical keys.
    pub sequence: &'static [&'static [ferrokey_core::PhysicalKey]],
}

use ferrokey_core::PhysicalKey as K;

/// The generic terminal row: the shell-independent controls. Used for
/// `Unknown`, for ssh (remote shell unknowable) and as the fallback layer
/// underneath every shell row.
pub const GENERIC_ROW: &[ShellRowKey] = &[
    ShellRowKey {
        label: "Ctrl+C",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::C]],
    },
    ShellRowKey {
        label: "Ctrl+D",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::D]],
    },
    ShellRowKey {
        label: "Ctrl+Z",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::Z]],
    },
    ShellRowKey {
        label: "Ctrl+L",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::L]],
    },
    ShellRowKey {
        label: "Ctrl+A",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::A]],
    },
    ShellRowKey {
        label: "Esc",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::Escape]],
    },
    ShellRowKey {
        label: "Home",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::Home]],
    },
    ShellRowKey {
        label: "End",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::End]],
    },
];

/// The bash row: real Readline bindings (§5.6).
///
/// Every chord is a default Readline binding in stock bash: reverse-search
/// history, kill-line, yank, unix-line-discard, backward-kill-word, and the
/// classic beginning/end-of-line. `Ctrl+X Ctrl+E` opens the command in the
/// editor (Readline default `edit-command-line`).
pub const BASH_ROW: &[ShellRowKey] = &[
    ShellRowKey {
        label: "Ctrl+R",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::R]],
    },
    ShellRowKey {
        label: "Ctrl+K",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::K]],
    },
    ShellRowKey {
        label: "Ctrl+Y",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::Y]],
    },
    ShellRowKey {
        label: "Ctrl+U",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::U]],
    },
    ShellRowKey {
        label: "Ctrl+W",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::W]],
    },
    ShellRowKey {
        label: "Ctrl+A",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::A]],
    },
    ShellRowKey {
        label: "Ctrl+E",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::E]],
    },
    // "Ctrl+X Ctrl+E" (readline edit-command-line) is the longest label in
    // the row; it needs a wider key or it would be cropped at the default
    // width.
    ShellRowKey {
        label: "Ctrl+X Ctrl+E",
        width: 2.4,
        sequence: &[&[K::LeftCtrl, K::X], &[K::LeftCtrl, K::E]],
    },
];

/// The zsh row: real zle default bindings (§5.7).
///
/// `Ctrl+R` is the zle default `history-incremental-search-backward`;
/// `Ctrl+A/E/U/K/W/Y` are default zle vi/emacs-mode bindings
/// (beginning/end-of-line, kill whole line, kill-line, backward-kill-word,
/// yank). `Ctrl+X Ctrl+E` is `edit-command-line` — **not** bound by default
/// in zsh; the standard `autoload -U edit-command-line; zle -N
/// edit-command-line; bindkey '^X^E' edit-command-line` wiring is required.
/// Directory-stack navigation has **no default zle bindings**, so no
/// directory-stack buttons are offered (representing configuration-dependent
/// shortcuts honestly).
pub const ZSH_ROW: &[ShellRowKey] = &[
    ShellRowKey {
        label: "Ctrl+R",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::R]],
    },
    ShellRowKey {
        label: "Ctrl+A",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::A]],
    },
    ShellRowKey {
        label: "Ctrl+E",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::E]],
    },
    ShellRowKey {
        label: "Ctrl+U",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::U]],
    },
    ShellRowKey {
        label: "Ctrl+K",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::K]],
    },
    ShellRowKey {
        label: "Ctrl+W",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::W]],
    },
    ShellRowKey {
        label: "Ctrl+Y",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::Y]],
    },
    // Same wide key as the bash row: the label does not fit at the default
    // width.
    ShellRowKey {
        label: "Ctrl+X Ctrl+E",
        width: 2.4,
        sequence: &[&[K::LeftCtrl, K::X], &[K::LeftCtrl, K::E]],
    },
];

/// The fish row: real fish bindings (§5.8).
///
/// Autosuggestion acceptance: the right arrow (fish's default
/// `accept-autosuggestion` when a suggestion exists) and `Ctrl+F`
/// (also `accept-autosuggestion` by default). Completion is `Tab`;
/// history is Up/Down. `Ctrl+U` (`backward-kill-line`) and `Ctrl+W`
/// (`backward-kill-path-component`) are fish defaults. Abbreviations expand
/// when the abbreviation text is followed by space/enter — plain typing,
/// no special button (there is no default key that triggers expansion).
pub const FISH_ROW: &[ShellRowKey] = &[
    ShellRowKey {
        label: "Tab",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::Tab]],
    },
    // The arrow glyph plus "accept" needs a wider key than the default
    // (its label would be cropped at 1.25).
    ShellRowKey {
        label: "→ accept",
        width: 1.9,
        sequence: &[&[K::Right]],
    },
    ShellRowKey {
        label: "Ctrl+F",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::F]],
    },
    ShellRowKey {
        label: "Up",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::Up]],
    },
    ShellRowKey {
        label: "Down",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::Down]],
    },
    ShellRowKey {
        label: "Ctrl+U",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::U]],
    },
    ShellRowKey {
        label: "Ctrl+W",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::W]],
    },
];

/// The Nushell row: real reedline default bindings (§5.9).
///
/// Nushell's line editor (reedline) binds `Ctrl+R` to history search,
/// `Ctrl+A`/`Ctrl+E` to line start/end, `Ctrl+U` to clear, `Tab` to
/// completion, and Up/Down to history. Only these verified defaults are
/// offered — no invented function-key shortcuts.
pub const NUSHELL_ROW: &[ShellRowKey] = &[
    ShellRowKey {
        label: "Ctrl+R",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::R]],
    },
    ShellRowKey {
        label: "Ctrl+A",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::A]],
    },
    ShellRowKey {
        label: "Ctrl+E",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::E]],
    },
    ShellRowKey {
        label: "Ctrl+U",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::LeftCtrl, K::U]],
    },
    ShellRowKey {
        label: "Tab",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::Tab]],
    },
    ShellRowKey {
        label: "Up",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::Up]],
    },
    ShellRowKey {
        label: "Down",
        width: SHELL_KEY_WIDTH,
        sequence: &[&[K::Down]],
    },
];

/// The tmux row: **key-sequence** controls behind the default `Ctrl+B`
/// prefix (§5.11). Every action is a keyboard chord through the normal
/// encoder — never tmux IPC, never a hidden command.
pub const TMUX_ROW: &[ShellRowKey] = &[
    // The "prefix <key>" labels are longer than the generic chord labels, so
    // every tmux key is wider than the default (labels would crop at 1.25).
    ShellRowKey {
        label: "prefix d",
        width: 1.6,
        sequence: &[&[K::LeftCtrl, K::B], &[K::D]],
    },
    ShellRowKey {
        label: "prefix c",
        width: 1.6,
        sequence: &[&[K::LeftCtrl, K::B], &[K::C]],
    },
    ShellRowKey {
        label: "prefix n",
        width: 1.6,
        sequence: &[&[K::LeftCtrl, K::B], &[K::N]],
    },
    ShellRowKey {
        label: "prefix %",
        width: 1.6,
        sequence: &[&[K::LeftCtrl, K::B], &[K::LeftShift, K::D5]],
    },
    ShellRowKey {
        label: "prefix \"",
        width: 1.6,
        sequence: &[&[K::LeftCtrl, K::B], &[K::LeftShift, K::Apostrophe]],
    },
];

/// Resolve a row id to its key table (generic for unknown/ssh).
pub fn shell_row(id: &str) -> &'static [ShellRowKey] {
    match id {
        "bash" => BASH_ROW,
        "zsh" => ZSH_ROW,
        "fish" => FISH_ROW,
        "nushell" => NUSHELL_ROW,
        "tmux" => TMUX_ROW,
        _ => GENERIC_ROW,
    }
}

/// Encode one shell-row key's sequence to the exact PTY bytes the child
/// receives, through the **real** encoder (WS5 §5.14). Each press-group is
/// encoded with its members' modifiers applied (the encoder's responsibility
/// is the current held set — here we model the group in isolation, which is
/// exactly how the bridge plays a sequence).
pub fn encode_sequence(
    encoder: &TerminalKeyEncoder,
    sequence: &[&[ferrokey_core::PhysicalKey]],
) -> Vec<u8> {
    use ferrokey_core::ModifierSet;
    let mut out = Vec::new();
    let term_modes = crate::modes::TerminalModes::default();
    for group in sequence {
        let mut held_mods = ModifierSet::empty();
        for &key in *group {
            if let Some(kind) = key.modifier_kind() {
                held_mods = held_mods.union(kind.into());
            }
        }
        for &key in *group {
            if let Some(bytes) = encoder.encode(key, held_mods, &term_modes) {
                out.extend_from_slice(&bytes);
            }
        }
    }
    out
}
