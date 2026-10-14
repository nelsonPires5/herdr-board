//! Foreground commands for config-defined card actions (`[[card_action]]`),
//! isolated behind a trait so tests can fake them (same shape as
//! [`crate::editor`]). The child environment is sanitized at this boundary:
//! inherited actor credentials are removed and the fresh `BOARD_ACTION_*`
//! context is applied before the process is spawned.

use std::path::PathBuf;

/// One card action to run: the configured argv plus the card context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    pub argv: Vec<String>,
    /// `BOARD_ACTION_*` variables naming the card the action was run on.
    pub env: Vec<(String, String)>,
    /// The card's space directory. `None` (a card with no space) means the
    /// action runs in the TUI's own cwd; a space-associated card always
    /// carries an explicit path here (the app refuses otherwise).
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

/// Inherited actor-credential variables that must never leak into a card
/// action: `board comment`/`board done` would otherwise attribute the action
/// to the agent run (or pane) that happened to launch the TUI.
const INHERITED_ACTOR_ENV: [&str; 3] = ["BOARD_CARD_ID", "BOARD_RUN_ID", "HERDR_PANE_ID"];

/// Namespaced action-context variables; stale values (notably an optional
/// `BOARD_ACTION_SPACE_REF` from an outer action) must be cleared before the
/// fresh spec is applied.
const BOARD_ACTION_ENV: [&str; 4] = [
    "BOARD_ACTION_CARD_ID",
    "BOARD_ACTION_BOARD_ID",
    "BOARD_ACTION_COLUMN_ID",
    "BOARD_ACTION_SPACE_REF",
];

/// Strip inherited actor credentials and stale action context from `command`,
/// then apply the fresh `spec.env`. Ordinary variables (`PATH`, `HOME`, the
/// board/herdr sockets) are untouched: the child must still reach the daemon
/// and the user's tools.
fn sanitize_card_action_env(command: &mut std::process::Command, spec: &CommandSpec) {
    for key in INHERITED_ACTOR_ENV {
        command.env_remove(key);
    }
    for key in BOARD_ACTION_ENV {
        command.env_remove(key);
    }
    command.envs(spec.env.iter().cloned());
}

/// Build the child command with a sanitized environment. Kept separate so the
/// env contract can be asserted without spawning a real process.
fn build_card_action_command(spec: &CommandSpec) -> anyhow::Result<std::process::Command> {
    let (program, args) = spec
        .argv
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("card action has an empty argv"))?;
    let mut command = std::process::Command::new(program);
    command.args(args);
    sanitize_card_action_env(&mut command, spec);
    // A configured cwd is passed through verbatim: an invalid/removed path
    // must fail the spawn (and surface as an error), never silently fall back
    // to the TUI's own working directory.
    if let Some(cwd) = spec.cwd.as_ref() {
        command.current_dir(cwd);
    }
    Ok(command)
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

        let mut command = build_card_action_command(spec)?;

        // Suspend the TUI.
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();

        let status = command.status();

        // Resume the TUI regardless of the command's exit status.
        let _ = enable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), EnterAlternateScreen, EnableMouseCapture);

        let program = spec.argv.first().map_or("card action", String::as_str);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn spec(argv: &[&str], env: &[(&str, &str)], cwd: Option<&str>) -> CommandSpec {
        CommandSpec {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            env: env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            cwd: cwd.map(PathBuf::from),
        }
    }

    fn dump_env(command: &mut std::process::Command) -> HashMap<String, String> {
        let output = command.output().expect("spawn env");
        assert!(output.status.success());
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn child_environment_drops_actor_credentials_and_stale_action_context() {
        // Simulate a TUI launched from inside an agent pane: the inherited
        // environment carries actor credentials plus an outer action's context.
        let mut command = std::process::Command::new("sh");
        command.arg("-c").arg("env");
        for (k, v) in [
            ("BOARD_CARD_ID", "999"),
            ("BOARD_RUN_ID", "run-abc"),
            ("HERDR_PANE_ID", "pane-1"),
            ("BOARD_ACTION_CARD_ID", "stale"),
            ("BOARD_ACTION_BOARD_ID", "stale"),
            ("BOARD_ACTION_COLUMN_ID", "stale"),
            ("BOARD_ACTION_SPACE_REF", "stale-space"),
            ("BOARD_SOCKET", "/tmp/board.sock"),
            ("HERDR_SOCKET_PATH", "/tmp/herdr.sock"),
        ] {
            command.env(k, v);
        }
        let spec = spec(
            &["unused"],
            &[
                ("BOARD_ACTION_CARD_ID", "42"),
                ("BOARD_ACTION_BOARD_ID", "7"),
                ("BOARD_ACTION_COLUMN_ID", "3"),
            ],
            None,
        );
        sanitize_card_action_env(&mut command, &spec);
        let env = dump_env(&mut command);

        for gone in [
            "BOARD_CARD_ID",
            "BOARD_RUN_ID",
            "HERDR_PANE_ID",
            "BOARD_ACTION_SPACE_REF",
        ] {
            assert!(!env.contains_key(gone), "{gone} leaked into the child env");
        }
        assert_eq!(env["BOARD_ACTION_CARD_ID"], "42");
        assert_eq!(env["BOARD_ACTION_BOARD_ID"], "7");
        assert_eq!(env["BOARD_ACTION_COLUMN_ID"], "3");
        // Ordinary board/herdr routing variables survive.
        assert_eq!(env["BOARD_SOCKET"], "/tmp/board.sock");
        assert_eq!(env["HERDR_SOCKET_PATH"], "/tmp/herdr.sock");
    }

    #[test]
    fn build_command_keeps_argv_literal_and_cwd_verbatim() {
        let command = build_card_action_command(&spec(
            &["/bin/echo", "a b", "c"],
            &[],
            Some("/definitely/not/a/real/dir"),
        ))
        .unwrap();
        assert_eq!(command.get_program(), "/bin/echo");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["a b", "c"]);
        assert_eq!(
            command.get_current_dir(),
            Some(std::path::Path::new("/definitely/not/a/real/dir"))
        );
    }

    #[test]
    fn missing_configured_cwd_fails_instead_of_falling_back() {
        let mut command = build_card_action_command(&spec(
            &["sh", "-c", "pwd"],
            &[],
            Some("/definitely/not/a/real/dir"),
        ))
        .unwrap();
        let err = command.output().expect_err("spawn must fail");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn file_configured_cwd_fails_instead_of_falling_back() {
        // A cwd that names a file (not a directory) must fail the spawn:
        // `current_dir` is passed through verbatim, never filtered away.
        let mut command =
            build_card_action_command(&spec(&["sh", "-c", "pwd"], &[], Some("/bin/sh"))).unwrap();
        assert!(command.output().is_err(), "file cwd must fail the spawn");
    }

    #[test]
    fn no_configured_cwd_leaves_the_parent_directory_to_the_child() {
        let command = build_card_action_command(&spec(&["/bin/echo", "hi"], &[], None)).unwrap();
        assert_eq!(command.get_current_dir(), None);
    }

    #[test]
    fn empty_argv_is_rejected() {
        let err = build_card_action_command(&spec(&[], &[], None)).unwrap_err();
        assert!(err.to_string().contains("empty argv"));
    }
}
