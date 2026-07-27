//! Run subcommands (`board card run …`).

use clap::Subcommand;

#[derive(Subcommand)]
pub(crate) enum RunCmd {
    /// Close the active run.
    Done {
        card_id: Option<i64>,
        #[arg(long, value_parser = ["ok", "fail"])]
        outcome: String,
        #[arg(long)]
        summary: Option<String>,
    },
    /// Cancel the active run.
    Cancel { card_id: i64 },
    /// Retry the card in its current column.
    Retry { card_id: i64 },
    /// Focus one exact run's pane (the run id is required).
    Focus {
        card_id: i64,
        run_id: i64,
        #[arg(long)]
        origin_socket: Option<String>,
    },
    /// Confirm an awaiting run (same completion channel as `run done --outcome ok`).
    Confirm {
        card_id: Option<i64>,
        #[arg(long)]
        summary: Option<String>,
    },
}
