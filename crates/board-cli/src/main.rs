//! board — the single CLI binary.
//!
//! clap subcommands over the boardd socket: `tui`, `daemon`, `status`, and the
//! agent/user verbs (`card`, `comment`, `done`, `move`, `cancel`, `retry`,
//! `column`). Every connecting command auto-starts the daemon if it is absent.

mod args;
mod commands;
mod daemon;
mod helpers;
mod scope;

use anyhow::{anyhow, Result};
use clap::Parser;

use args::{Cli, Cmd};
use commands::card::cmd_card;
use commands::column::cmd_column;
use commands::discovery::{cmd_harness, cmd_session, cmd_space, cmd_status};
use commands::run::{cmd_comment, cmd_done, cmd_move, cmd_pane_exited, cmd_run_action};
use daemon::{connect_or_start, stop_daemon};
use scope::open_current_board;

fn main() {
    if let Err(e) = real_main() {
        eprintln!("board: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Daemon { foreground, stop } => {
            if stop {
                stop_daemon()
            } else {
                board_daemon::run(foreground).map_err(|e| anyhow!(e))
            }
        }
        Cmd::Tui => {
            let mut client = connect_or_start()?;
            let board = open_current_board(&mut client)?;
            board_tui::run_with_board(Box::new(client), board)
        }
        Cmd::Status { json } => cmd_status(json),
        Cmd::Card { sub } => cmd_card(sub),
        Cmd::Comment { first, body, json } => cmd_comment(first, body, json),
        Cmd::Done {
            card_id,
            outcome,
            summary,
            json,
        } => cmd_done(card_id, outcome, summary, json),
        Cmd::PaneExited { card_id, run_id } => cmd_pane_exited(card_id, run_id),
        Cmd::Move {
            card_id,
            column,
            json,
        } => cmd_move(card_id, column, json),
        Cmd::Cancel { card_id, json } => cmd_run_action(card_id, json, false),
        Cmd::Retry { card_id, json } => cmd_run_action(card_id, json, true),
        Cmd::Column { sub } => cmd_column(sub),
        Cmd::Harness { sub } => cmd_harness(sub),
        Cmd::Space { sub } => cmd_space(sub),
        Cmd::Session { sub } => cmd_session(sub),
    }
}
