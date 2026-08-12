//! Card and comment subcommands.

use clap::Subcommand;

use super::{ConfirmArgs, RunCmd};

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum CardCmd {
    /// Create a card (`new` is retained as an alias).
    #[command(alias = "new")]
    Create {
        #[arg(long)]
        title: String,
        #[arg(long, short = 'd')]
        description: Option<String>,
        #[arg(long)]
        column: Option<String>,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        permission: Option<String>,
        /// herdr session name (default: the daemon's default session).
        #[arg(long)]
        session: Option<String>,
        /// Space kind: `workspace` or `new-workspace`.
        #[arg(long)]
        space_kind: Option<String>,
        #[arg(long)]
        space_ref: Option<String>,
        /// Working directory for a `new-workspace` space.
        #[arg(long)]
        space_cwd: Option<String>,
    },
    /// Update card fields; nullable fields are changed only with explicit clear flags.
    Edit {
        id: i64,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, short = 'd')]
        description: Option<String>,
        #[arg(long)]
        clear_description: bool,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        clear_harness: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        clear_model: bool,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        clear_effort: bool,
        #[arg(long)]
        permission: Option<String>,
        #[arg(long)]
        clear_permission: bool,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        clear_session: bool,
        #[arg(long)]
        space_ref: Option<String>,
        #[arg(long)]
        clear_space_ref: bool,
        #[arg(long)]
        space_cwd: Option<String>,
        #[arg(long)]
        clear_space_cwd: bool,
    },
    /// Permanently delete a card and its history.
    Delete {
        id: i64,
        #[command(flatten)]
        confirm: ConfirmArgs,
    },
    /// Archive an idle/done/failed card without deleting its history.
    Archive { id: i64 },
    /// Restore an archived card to the active board.
    Restore { id: i64 },
    /// Show a card with comments and run history.
    Show { id: i64 },
    /// List cards (optionally filtered by column and visibility).
    List {
        #[arg(long)]
        column: Option<String>,
        #[arg(long, value_parser = ["active", "all", "archived"])]
        visibility: Option<String>,
    },
    /// Move a card, optionally across boards. With `--position`, the card is
    /// reordered within its current column (same-column move, never triggers
    /// an automatic column).
    Move {
        id: i64,
        column: String,
        /// Zero-based index to place the card at within the destination column.
        #[arg(long)]
        position: Option<i64>,
        #[arg(long, alias = "to-board", value_name = "ID|PATH")]
        destination_board: Option<String>,
    },
    /// Nested comment operations.
    Comment {
        #[command(subcommand)]
        sub: CommentCmd,
    },
    /// Nested run operations.
    Run {
        #[command(subcommand)]
        sub: RunCmd,
    },
}

#[derive(Subcommand)]
pub(crate) enum CommentCmd {
    /// Add a comment to a card.
    Add { card_id: i64, body: String },
    /// Show a comment by id.
    Show { comment_id: i64 },
    /// Edit a comment body.
    Edit { comment_id: i64, body: String },
    /// Soft-delete a comment.
    Delete {
        comment_id: i64,
        #[command(flatten)]
        confirm: ConfirmArgs,
    },
    /// Show a comment's audit history.
    History { comment_id: i64 },
}
