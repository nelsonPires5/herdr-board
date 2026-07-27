//! Board and template subcommands.

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum BoardCmd {
    /// List all boards.
    List,
    /// Show one board by id, path, or the selected/current board.
    Show { selector: Option<String> },
    /// Open (or create) the board for a scope path.
    Open { path: String },
    /// Rename a board by id, path, or the selected/current board.
    Rename {
        /// `<ID|PATH> <NAME>` or `<NAME>` with global --board/current scope.
        #[arg(required = true, num_args = 1..=2)]
        values: Vec<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum TemplateCmd {
    /// Apply a named template to the selected board.
    Apply { name: String },
}
