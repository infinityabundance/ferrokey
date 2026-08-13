//! Clipboard integration (§28, §73–§74): a trait the host application
//! implements.
//!
//! Copying terminal text goes through the **unprivileged UI layer** — never
//! through `ferrokeyd` or any privileged component, and the content is never
//! logged (§28, §79). OSC 52 clipboard read/write from terminal applications
//! is **denied by default** in the terminal itself (see [`crate::terminal`]);
//! this trait is only for user-initiated copy/paste actions.

/// Errors from the clipboard backend.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClipboardError {
    #[error("clipboard backend unavailable: {0}")]
    Unavailable(String),
    #[error("clipboard operation failed: {0}")]
    Failed(String),
}

/// A clipboard the UI can read and write.
pub trait Clipboard {
    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError>;
    fn get_text(&mut self) -> Result<String, ClipboardError>;
}

/// A clipboard that always fails — the safe default when no backend exists.
/// The UI reports the error; nothing is silently dropped or logged.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoClipboard;

impl Clipboard for NoClipboard {
    fn set_text(&mut self, _text: &str) -> Result<(), ClipboardError> {
        Err(ClipboardError::Unavailable(
            "no clipboard backend configured".into(),
        ))
    }
    fn get_text(&mut self) -> Result<String, ClipboardError> {
        Err(ClipboardError::Unavailable(
            "no clipboard backend configured".into(),
        ))
    }
}

/// A best-effort clipboard that shells out to the desktop's standard tools
/// (`wl-copy`/`wl-paste` on Wayland, `xclip`/`xsel` on X11). This is a
/// pragmatic fallback for the unprivileged UI; a native X11/Wayland
/// selection implementation is the long-term replacement.
#[derive(Debug, Clone, Default)]
pub struct ExternalClipboard {
    copy_cmd: Option<String>,
    paste_cmd: Option<String>,
}

impl ExternalClipboard {
    pub fn detect() -> Self {
        let copy_cmd = ["wl-copy", "xclip", "xsel"]
            .iter()
            .find(|cmd| which(cmd))
            .map(std::string::ToString::to_string);
        let paste_cmd = if copy_cmd.as_deref() == Some("wl-copy") {
            Some("wl-paste".to_string())
        } else if copy_cmd.as_deref() == Some("xclip") {
            Some("xclip -o".to_string())
        } else if copy_cmd.as_deref() == Some("xsel") {
            Some("xsel -b".to_string())
        } else {
            None
        };
        ExternalClipboard {
            copy_cmd,
            paste_cmd,
        }
    }
}

impl Clipboard for ExternalClipboard {
    fn set_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        use std::io::Write;
        let Some(cmd) = &self.copy_cmd else {
            return Err(ClipboardError::Unavailable(
                "no clipboard tool found (wl-copy/xclip/xsel)".into(),
            ));
        };
        let mut child = std::process::Command::new(cmd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| ClipboardError::Failed(e.to_string()))?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let status = child
            .wait()
            .map_err(|e| ClipboardError::Failed(e.to_string()))?;
        if status.success() {
            Ok(())
        } else {
            Err(ClipboardError::Failed(format!(
                "{cmd} exited with {status}"
            )))
        }
    }

    fn get_text(&mut self) -> Result<String, ClipboardError> {
        let Some(cmd) = &self.paste_cmd else {
            return Err(ClipboardError::Unavailable(
                "no clipboard tool found (wl-paste/xclip/xsel)".into(),
            ));
        };
        // Split the command (e.g. "xclip -o") into program + args.
        let mut parts = cmd.split_whitespace();
        let program = parts
            .next()
            .ok_or_else(|| ClipboardError::Unavailable("empty paste command".into()))?;
        let args: Vec<&str> = parts.collect();
        let output = std::process::Command::new(program)
            .args(args)
            .output()
            .map_err(|e| ClipboardError::Failed(e.to_string()))?;
        if output.status.success() {
            // Bounded paste: the terminal enforces MAX_PASTE_BYTES too.
            let text = String::from_utf8_lossy(&output.stdout).into_owned();
            Ok(text)
        } else {
            Err(ClipboardError::Failed(format!(
                "{program} exited with {}",
                output.status
            )))
        }
    }
}

fn which(cmd: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(cmd))
                .any(|p| p.is_file())
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_clipboard_fails_safely() {
        let mut c = NoClipboard;
        assert!(c.set_text("x").is_err());
        assert!(c.get_text().is_err());
    }

    #[test]
    fn detect_finds_or_returns_unavailable() {
        // On the dev host this may find wl-copy; in the VM it may not. Either
        // way the type must behave consistently.
        let mut c = ExternalClipboard::detect();
        if c.copy_cmd.is_some() {
            assert!(c.paste_cmd.is_some());
        }
        // Operations never panic regardless.
        let _ = c.set_text("test");
        let _ = c.get_text();
    }
}
