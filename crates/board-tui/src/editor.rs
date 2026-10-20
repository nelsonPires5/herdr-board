//! `$EDITOR` suspend/resume, isolated behind a trait so tests can fake it.

use std::io::{Read, Write};

/// Result of an editor round-trip: the (possibly edited) text, plus whether
/// the terminal needs a full repaint before the next `draw` call.
///
/// Bug A: `RealEditor` leaves the alternate screen for a *different* stdout
/// handle than the one `ratatui::Terminal` owns; ratatui's buffer diff then
/// has no idea the screen was ever clobbered by `$EDITOR`; and skips redrawing
/// unchanged cells, leaving the screen blank. `needs_full_redraw: true` tells
/// the driver to call `terminal.clear()` before the next draw so every cell is
/// repainted regardless of the diff.
pub struct EditResult {
    pub text: String,
    pub needs_full_redraw: bool,
}

/// Launches an external editor on some initial text, returning the edited text.
///
/// The real implementation suspends the TUI (leaves the alternate screen and
/// disables raw mode), spawns `$EDITOR` on a tempfile, then restores the
/// terminal. Tests provide a fake that returns canned text without any I/O.
pub trait EditorLauncher {
    fn edit(&self, initial: &str) -> anyhow::Result<EditResult>;
}

/// Editor used when neither `$EDITOR` nor `$VISUAL` is set: `vi` on unix,
/// `notepad` on Windows (always present, and blocks until the window closes).
const DEFAULT_EDITOR: &str = if cfg!(windows) { "notepad" } else { "vi" };

/// Pick the editor program: `$EDITOR`, else `$VISUAL`, else
/// [`DEFAULT_EDITOR`]. Takes the raw env values so it stays pure.
fn editor_program(editor: Option<String>, visual: Option<String>) -> String {
    editor
        .or(visual)
        .unwrap_or_else(|| DEFAULT_EDITOR.to_string())
}

/// Production launcher: `$EDITOR` (fallback `vi`, `notepad` on Windows) on a
/// tempfile.
pub struct RealEditor;

impl EditorLauncher for RealEditor {
    fn edit(&self, initial: &str) -> anyhow::Result<EditResult> {
        use crossterm::terminal::{
            disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
        };

        let editor = editor_program(std::env::var("EDITOR").ok(), std::env::var("VISUAL").ok());

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

        // Resolved like every other spawned program, so `code.cmd`-style
        // shims work on Windows; unix keeps `Command`'s own PATH search.
        let status = board_core::exec::command(&editor).arg(&path).status();

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
        Ok(EditResult {
            text: edited,
            needs_full_redraw: true,
        })
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
    fn edit(&self, _initial: &str) -> anyhow::Result<EditResult> {
        Ok(EditResult {
            text: self.result.clone(),
            needs_full_redraw: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_env_wins_over_visual_then_platform_default() {
        assert_eq!(
            editor_program(Some("nano".into()), Some("code".into())),
            "nano"
        );
        assert_eq!(editor_program(None, Some("code".into())), "code");
        let fallback = editor_program(None, None);
        if cfg!(windows) {
            assert_eq!(fallback, "notepad");
        } else {
            assert_eq!(fallback, "vi");
        }
    }
}
