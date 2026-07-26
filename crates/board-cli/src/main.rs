//! board — the single CLI binary.

mod args;
mod commands;
mod daemon;
mod helpers;
mod scope;

use std::io::Write;

use anyhow::{anyhow, Result};
use board_core::client::{BoardClient, RpcClientError, UnixClient};
use clap::{error::ErrorKind, Parser};
use serde_json::{json, Value};

use args::{Cli, Cmd};
use commands::board::cmd_board;
use commands::card::{cmd_card, cmd_move};
use commands::column::cmd_column;
use commands::discovery::{cmd_harness, cmd_session, cmd_space, cmd_status};
use commands::run::{cmd_comment, cmd_done, cmd_pane_exited, cmd_run_action};
use commands::template::cmd_template;
use daemon::{connect_or_start, stop_daemon};
use scope::open_selected_board;

fn main() {
    let json_requested = std::env::args().any(|arg| arg == "--json");
    if let Err(error) = real_main() {
        render_error(error, json_requested);
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            print!("{error}");
            return Ok(());
        }
        Err(error) => return Err(anyhow!(error.to_string())),
    };
    let selector = cli.board.as_deref();
    match cli.cmd {
        Cmd::Daemon {
            foreground,
            stop,
            sub,
        } => match sub {
            Some(args::DaemonCmd::Status { json }) => cmd_status(json),
            None => {
                if stop {
                    stop_daemon()
                } else {
                    board_daemon::run(foreground).map_err(|e| anyhow!(e))
                }
            }
        },
        Cmd::Tui => {
            let mut client = connect_or_start()?;
            let board = open_selected_board(&mut client, selector)?;
            board_tui::run_with_board(Box::new(client), board)
        }
        Cmd::Version { json } => cmd_version(json),
        Cmd::Skill => print_skill(),
        Cmd::Board { sub } => cmd_board(sub, selector),
        Cmd::Template { sub } => cmd_template(sub, selector),
        Cmd::Card { sub } => cmd_card(sub, selector),
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
            destination_board,
            json,
        } => {
            let destination = destination_board.as_deref().or(selector);
            let mut client = connect_or_start()?;
            cmd_move(&mut client, card_id, &column, destination, json)
        }
        Cmd::Cancel { card_id, json } => cmd_run_action(card_id, json, false),
        Cmd::Retry { card_id, json } => cmd_run_action(card_id, json, true),
        Cmd::Column { sub } => cmd_column(sub, selector),
        Cmd::Harness { sub } => cmd_harness(sub),
        Cmd::Space { sub } => cmd_space(sub),
        Cmd::Session { sub } => cmd_session(sub),
    }
}

fn cmd_version(json_output: bool) -> Result<()> {
    let daemon_version = match UnixClient::connect(&board_core::paths::socket_path()) {
        Ok(mut client) => client.daemon_status().ok().map(|status| status.version),
        Err(_) => None,
    };
    if json_output {
        print_json(&json!({
            "cli_version": env!("CARGO_PKG_VERSION"),
            "daemon_version": daemon_version,
        }))
    } else {
        println!("cli {}", env!("CARGO_PKG_VERSION"));
        println!(
            "daemon {}",
            daemon_version.as_deref().unwrap_or("unavailable")
        );
        Ok(())
    }
}

fn print_skill() -> Result<()> {
    std::io::stdout().write_all(include_bytes!("../../../skill/SKILL.md"))?;
    Ok(())
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn render_error(error: anyhow::Error, json_requested: bool) {
    if !json_requested {
        eprintln!("board: {error:#}");
        return;
    }

    let rpc = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RpcClientError>());
    let payload = if let Some(rpc) = rpc {
        let mut error_object = serde_json::Map::new();
        error_object.insert("code".into(), json!(rpc.code));
        if let Some(kind) = &rpc.kind {
            error_object.insert("kind".into(), json!(kind));
        }
        let message = if error.chain().count() > 1 {
            format!("{error:#}")
        } else {
            rpc.message.clone()
        };
        error_object.insert("message".into(), json!(message));
        if let Some(details) = &rpc.details {
            error_object.insert("details".into(), details.clone());
        }
        Value::Object({
            let mut root = serde_json::Map::new();
            root.insert("error".into(), Value::Object(error_object));
            root
        })
    } else {
        json!({
            "error": {
                "code": 2,
                "kind": "cli",
                "message": format!("{error:#}"),
            }
        })
    };
    eprintln!(
        "{}",
        serde_json::to_string(&payload).unwrap_or_else(|_| {
            r#"{"error":{"code":2,"message":"board command failed"}}"#.into()
        })
    );
}
