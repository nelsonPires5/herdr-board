//! Column subcommands.

use clap::Subcommand;

use super::ConfirmArgs;

#[derive(Subcommand)]
pub(crate) enum ColumnCmd {
    /// List columns.
    List,
    /// Create a column.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        trigger: Option<String>,
        #[arg(long)]
        on_success: Option<String>,
        #[arg(long)]
        on_fail: Option<String>,
        /// Start each run in a fresh herdr session (mutually exclusive with --reuse-session).
        #[arg(long, conflicts_with = "reuse_session")]
        fresh_session: bool,
        /// Reuse the card's herdr session (mutually exclusive with --fresh-session).
        #[arg(long)]
        reuse_session: bool,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        permission: Option<String>,
        #[arg(long)]
        timeout: Option<i64>,
        #[arg(long)]
        position: Option<i64>,
    },
    /// Show a column by id or name.
    Show { column: String },
    /// Edit a column; nullable settings require explicit clear flags.
    Edit {
        column: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
        #[arg(long)]
        clear_prompt: bool,
        #[arg(long)]
        trigger: Option<String>,
        #[arg(long)]
        on_success: Option<String>,
        #[arg(long)]
        clear_on_success: bool,
        #[arg(long)]
        on_fail: Option<String>,
        #[arg(long)]
        clear_on_fail: bool,
        /// Start each run in a fresh herdr session (mutually exclusive with --reuse-session).
        #[arg(long, conflicts_with = "reuse_session")]
        fresh_session: bool,
        /// Reuse the card's herdr session (mutually exclusive with --fresh-session).
        #[arg(long)]
        reuse_session: bool,
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
        timeout: Option<i64>,
        #[arg(long)]
        clear_timeout: bool,
    },
    /// Reorder a column by its zero-based position.
    Reorder { column: String, position: i64 },
    /// Delete a column.
    Delete {
        column: String,
        #[arg(long)]
        move_cards_to: Option<String>,
        #[command(flatten)]
        confirm: ConfirmArgs,
    },
}
