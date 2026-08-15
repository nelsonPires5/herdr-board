//! The clap surface, split by domain.
//!
//! Every public path is re-exported here, so `crate::args::X` keeps working for
//! all command handlers. Two rules hold across this module:
//!
//! * **Backward compatibility is mandatory** — this CLI is scripted by agents.
//!   New spellings are additive and old ones become (sometimes hidden) aliases.
//! * `--json` is one `global = true` flag on [`Cli`], accepted before or after
//!   the subcommand path, instead of being redeclared on every leaf command.

mod board;
mod card;
mod column;
mod common;
mod discovery;
mod project;
mod run;

pub(crate) use board::{BoardCmd, TemplateCmd};
pub(crate) use card::{CardCmd, CommentCmd};
pub(crate) use column::ColumnCmd;
pub(crate) use common::ConfirmArgs;
pub(crate) use discovery::{HarnessCmd, SessionCmd, SpaceCmd};
pub(crate) use project::ProjectCmd;
pub(crate) use run::RunCmd;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "board", version, about = "herdr-board kanban for agents")]
pub(crate) struct Cli {
    /// Select a board by stable id or canonical scope path.
    #[arg(long, global = true, value_name = "ID|PATH")]
    pub(crate) board: Option<String>,
    /// Select a project by canonical scope path (updates the persistent selection).
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) project: Option<String>,
    /// Emit JSON on stdout, and JSON errors on stderr.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Cmd {
    /// Open the kanban TUI (auto-starts the daemon).
    Tui,
    /// Run or inspect the daemon.
    Daemon {
        /// Deprecated: use `board daemon start --foreground`.
        #[arg(long, hide = true)]
        foreground: bool,
        /// Deprecated: use `board daemon stop`.
        #[arg(long, hide = true)]
        stop: bool,
        #[command(subcommand)]
        sub: Option<DaemonCmd>,
    },
    /// Report CLI and daemon versions without starting the daemon.
    Version,
    /// Print the exact operational skill document.
    Skill,
    /// Board operations.
    Board {
        #[command(subcommand)]
        sub: BoardCmd,
    },
    /// Project operations.
    Project {
        #[command(subcommand)]
        sub: ProjectCmd,
    },
    /// Apply a board template.
    Template {
        #[command(subcommand)]
        sub: TemplateCmd,
    },
    /// Card operations.
    Card {
        #[command(subcommand)]
        sub: CardCmd,
    },
    /// Add a comment (`board comment [CARD_ID] BODY`; CARD_ID defaults to $BOARD_CARD_ID).
    Comment {
        /// Either the card id (when BODY follows) or the body (uses $BOARD_CARD_ID).
        first: String,
        /// The comment body, if a card id was given.
        body: Option<String>,
    },
    /// Close the active run (`board done [CARD_ID] --outcome ok|fail`).
    Done {
        /// Card id; defaults to $BOARD_CARD_ID.
        card_id: Option<i64>,
        #[arg(long, value_parser = ["ok", "fail"])]
        outcome: String,
        #[arg(long)]
        summary: Option<String>,
    },
    #[command(name = "__pane-exited", hide = true)]
    PaneExited {
        /// Card id; defaults to $BOARD_CARD_ID.
        card_id: Option<i64>,
        #[arg(long)]
        run_id: i64,
    },
    /// Move a card to a column (name, case-insensitive, or id). With
    /// `--position`, the card is reordered within its current column.
    Move {
        card_id: i64,
        column: String,
        /// Zero-based index to place the card at within the destination column.
        #[arg(long)]
        position: Option<i64>,
        /// Destination board for a cross-board move. The global --board is also
        /// accepted, but that fallback is deprecated.
        #[arg(long, alias = "to-board", value_name = "ID|PATH")]
        destination_board: Option<String>,
        /// Destination project for a cross-project move (canonical scope path).
        #[arg(long, value_name = "PATH")]
        to_project: Option<String>,
    },
    /// Cancel a card's run.
    Cancel { card_id: i64 },
    /// Retry a card (new forked run in its current column).
    Retry { card_id: i64 },
    /// Column operations.
    Column {
        #[command(subcommand)]
        sub: ColumnCmd,
    },
    /// Harness capability queries.
    Harness {
        #[command(subcommand)]
        sub: HarnessCmd,
    },
    /// Run-space (herdr workspace) operations.
    Space {
        #[command(subcommand)]
        sub: SpaceCmd,
    },
    /// herdr session operations.
    Session {
        #[command(subcommand)]
        sub: SessionCmd,
    },
}

/// The three daemon verbs. Bare `board daemon [--foreground|--stop]` keeps its
/// historical behavior through the hidden flags above.
#[derive(Subcommand)]
pub(crate) enum DaemonCmd {
    /// Run the daemon in this process.
    Start {
        /// Log to stderr as well as the log file, and stay attached.
        #[arg(long)]
        foreground: bool,
    },
    /// Stop the running daemon (graceful).
    Stop,
    /// Show operational daemon status.
    Status,
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
