//! Board and template subcommands.

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum BoardCmd {
    /// List boards: the selected project's, or every project's with --all.
    List {
        /// List every project's boards instead of the selected project's.
        #[arg(long)]
        all: bool,
        /// Select a project by path instead of the selected one.
        #[arg(long, value_name = "PATH")]
        project: Option<String>,
        /// Filter by archive state: active (default), all, or archived.
        #[arg(long, value_parser = ["active", "all", "archived"])]
        visibility: Option<String>,
    },
    /// Show one board by id, path, or the selected/current board.
    Show { selector: Option<String> },
    /// Open (or create) the board for a scope path.
    Open { path: String },
    /// Create a board in a project (auto-selected).
    Create {
        name: String,
        #[arg(long, value_name = "PATH")]
        project: Option<String>,
    },
    /// Select a board by id or name (updates the persistent selection).
    Select {
        selector: String,
        #[arg(long, value_name = "PATH")]
        project: Option<String>,
    },
    /// Rename a board by id, path, or name in the selected project, or the
    /// selected/current board.
    Rename {
        /// `<ID|PATH|NAME> <NAME>` or `<NAME>` with global --board/current scope.
        #[arg(required = true, num_args = 1..=2)]
        values: Vec<String>,
    },
    /// Archive a board (hidden by default; reversible).
    Archive { selector: Option<String> },
    /// Restore an archived board.
    Restore { selector: Option<String> },
}

#[derive(Subcommand)]
pub(crate) enum TemplateCmd {
    /// Apply a named template to the selected board.
    Apply { name: String },
}
