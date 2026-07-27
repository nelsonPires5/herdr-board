//! Shared `#[derive(Args)]` fragments.
//!
//! `--json` is deliberately **not** here: it is a single `global = true` flag on
//! [`Cli`](super::Cli), so one definition covers every subcommand and it is
//! accepted before or after the subcommand path. Parsing — not an argv scan —
//! decides whether output and errors are rendered as JSON.

use clap::Args;

/// The non-interactive confirmation shared by every destructive command.
#[derive(Args, Clone, Copy, Debug)]
pub(crate) struct ConfirmArgs {
    /// Confirm without prompting (required when stdin is not a TTY).
    #[arg(long)]
    pub(crate) yes: bool,
}
