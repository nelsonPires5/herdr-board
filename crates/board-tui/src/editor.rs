//! `$EDITOR` suspend/resume, isolated behind a trait so tests can fake it.

use std::io::{Read, Write};

/// Launches an external editor on some initial text, returning the edited text.
///
/// The real implementation suspends the TUI (leaves the alternate screen and
/// disables raw mode), spawns `$EDITOR` on a tempfile, then restores the
/// terminal. Tests provide a fake that returns canned text without any I/O.
pub trait EditorLauncher {
    fn edit(&self, initial: &str) -> anyhow::Result<String>;
}

/// Production launcher: `$EDITOR` (fallback `vi`) on a tempfile.
pub struct RealEditor;

impl EditorLauncher for RealEditor {
    fn edit(&self, initial: &str) -> anyhow::Result<String> {
        use crossterm::terminal::{
            disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
        };

        let editor = std::env::var("EDITOR")
            .or_else(|_| std::env::var("VISUAL"))
            .unwrap_or_else(|_| "vi".to_string());

        let mut tmp = tempfile::Builder::new()
            .prefix("board-edit-")
            .suffix(".md")
            .tempfile()?;
        tmp.write_all(initial.as_bytes())?;
        tmp.flush()?;
        let path = tmp.path().to_path_buf();

        // Suspend the TUI.
        let mut out = std::io::stdout();
        let _ = crossterm::execute!(out, LeaveAlternateScreen);
        let _ = disable_raw_mode();

        let status = std::process::Command::new(&editor).arg(&path).status();

        // Resume the TUI regardless of the editor's exit status.
        let _ = enable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), EnterAlternateScreen);

        status?; // surface a spawn failure as an error (after restoring the terminal)

        let mut edited = String::new();
        std::fs::File::open(&path)?.read_to_string(&mut edited)?;
        // Editors commonly append a trailing newline; trim a single one.
        if edited.ends_with('\n') {
            edited.pop();
            if edited.ends_with('\r') {
                edited.pop();
            }
        }
        Ok(edited)
    }
}

/// Test launcher: returns a fixed string, ignoring the input.
#[cfg(any(test, feature = "fake-client"))]
pub struct FakeEditor {
    pub result: String,
}

#[cfg(any(test, feature = "fake-client"))]
impl FakeEditor {
    pub fn new(result: impl Into<String>) -> Self {
        FakeEditor {
            result: result.into(),
        }
    }
}

#[cfg(any(test, feature = "fake-client"))]
impl EditorLauncher for FakeEditor {
    fn edit(&self, _initial: &str) -> anyhow::Result<String> {
        Ok(self.result.clone())
    }
}
