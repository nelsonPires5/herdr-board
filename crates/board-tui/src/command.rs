//! Foreground commands for config-defined card actions (`[[card_action]]`),
//! isolated behind a trait so tests can fake them (same shape as
//! [`crate::editor`]).

use std::path::PathBuf;

/// One card action to run: the configured argv plus the card context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub argv: Vec<String>,
    /// `BOARD_ACTION_*` variables naming the card the action was run on.
    pub env: Vec<(String, String)>,
    /// The card's space directory, when it has one (else the TUI's own cwd).
    pub cwd: Option<PathBuf>,
}

/// How a command round-trip ended.
pub struct CommandResult {
    /// `None` when the process was killed by a signal.
    pub exit_code: Option<i32>,
    pub success: bool,
    /// The TUI was suspended, so the next draw must repaint every cell (see
    /// [`crate::editor::EditResult`]).
    pub needs_full_redraw: bool,
}

/// Runs a card action in the foreground and waits for it.
pub trait CommandLauncher {
    fn run(&self, spec: &CommandSpec) -> anyhow::Result<CommandResult>;
}

/// Production launcher: suspends the TUI, runs the command attached to the
/// terminal, then restores the TUI.
pub struct RealCommand;

impl CommandLauncher for RealCommand {
    fn run(&self, spec: &CommandSpec) -> anyhow::Result<CommandResult> {
        use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
        use crossterm::terminal::{
            disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
        };

        let (program, args) = spec
            .argv
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("card action has an empty argv"))?;
        let mut command = std::process::Command::new(program);
        command.args(args).envs(spec.env.iter().cloned());
        if let Some(cwd) = spec.cwd.as_ref().filter(|p| p.is_dir()) {
            command.current_dir(cwd);
        }

        // Suspend the TUI.
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();

        let status = command.status();

        // Resume the TUI regardless of the command's exit status.
        let _ = enable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture);

        let status = status.map_err(|e| anyhow::anyhow!("{program}: {e}"))?;
        Ok(CommandResult {
            exit_code: status.code(),
            success: status.success(),
            needs_full_redraw: true,
        })
    }
}

/// Test launcher: records every spec and reports a fixed exit code.
#[cfg(any(test, feature = "fake-client"))]
#[derive(Clone, Default)]
pub struct FakeCommand {
    pub calls: std::rc::Rc<std::cell::RefCell<Vec<CommandSpec>>>,
    pub exit_code: i32,
}

#[cfg(any(test, feature = "fake-client"))]
impl CommandLauncher for FakeCommand {
    fn run(&self, spec: &CommandSpec) -> anyhow::Result<CommandResult> {
        self.calls.borrow_mut().push(spec.clone());
        Ok(CommandResult {
            exit_code: Some(self.exit_code),
            success: self.exit_code == 0,
            needs_full_redraw: false,
        })
    }
}
