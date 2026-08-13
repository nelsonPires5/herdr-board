//! board — the single CLI binary.

mod args;
mod commands;
mod context;
mod daemon;
mod helpers;
mod render;
mod scope;

use std::io::Write;

use anyhow::{anyhow, Result};
use board_core::client::{BoardClient, RpcClientError, UnixClient};
use clap::{error::ErrorKind, Parser};
use serde_json::{json, Value};

use args::{Cli, Cmd, DaemonCmd};
use commands::board::cmd_board;
use commands::card::{cmd_card, cmd_move};
use commands::column::cmd_column;
use commands::discovery::{cmd_harness, cmd_session, cmd_space, cmd_status};
use commands::run::{cmd_card_run, cmd_comment, cmd_pane_exited};
use commands::template::cmd_template;
use context::Ctx;
use daemon::stop_daemon;
use render::emit_line;

/// Exit code (and `--json` envelope `code`) for an error raised by the CLI
/// itself rather than by boardd: bad usage, a refused confirmation, a bad
/// environment. Deliberately outside the protocol's documented `1..=5` so it
/// cannot be confused with "not found" (`2`), which is what this used to
/// report. `64` is `EX_USAGE` from `sysexits.h`.
const CLI_ERROR_CODE: i32 = 64;

/// Exit code for an RPC error whose protocol code is outside `1..=5`. Protocol
/// codes are not exit codes: they are unbounded, while a process exit status is
/// taken modulo 256, so `256` would silently mean success. `70` is
/// `EX_SOFTWARE`.
const UNMAPPED_RPC_CODE: i32 = 70;

/// An error plus the rendering mode that was in force when it happened.
struct Failure {
    error: anyhow::Error,
    json: bool,
}

fn main() {
    if let Err(failure) = real_main() {
        let code = exit_code(&failure.error);
        render_error(&failure.error, failure.json);
        std::process::exit(code);
    }
}

fn real_main() -> Result<(), Failure> {
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
        Err(error) => {
            return Err(Failure {
                error: anyhow!(error.to_string()),
                // The parse failed, so there is no parsed flag to consult. Only
                // options can carry `--json` here, and everything after `--` is
                // a value, so a scan bounded by that separator is exact for the
                // arguments clap was still willing to interpret as flags.
                json: json_flag_in_options(),
            });
        }
    };
    let json = cli.json;
    dispatch(cli).map_err(|error| Failure { error, json })
}

fn dispatch(cli: Cli) -> Result<()> {
    let selector = cli.board.as_deref();
    let mut ctx = Ctx::new(selector, cli.json);
    match cli.cmd {
        Cmd::Daemon {
            foreground,
            stop,
            sub,
        } => match sub {
            Some(DaemonCmd::Status) => cmd_status(&mut ctx),
            Some(DaemonCmd::Stop) => stop_daemon(cli.json),
            Some(DaemonCmd::Start { foreground }) => run_daemon(foreground),
            // Bare `board daemon [--foreground|--stop]`: the pre-subcommand
            // grammar, kept working exactly as before.
            None if stop => stop_daemon(cli.json),
            None => run_daemon(foreground),
        },
        Cmd::Tui => {
            let board = ctx.board()?.clone();
            let client = ctx.into_client()?;
            board_tui::run_with_board(Box::new(client), board)
        }
        Cmd::Version => cmd_version(cli.json),
        Cmd::Skill => print_skill(),
        Cmd::Board { sub } => cmd_board(sub, &mut ctx),
        Cmd::Template { sub } => cmd_template(sub, &mut ctx),
        Cmd::Card { sub } => cmd_card(sub, &mut ctx),
        Cmd::Column { sub } => cmd_column(sub, &mut ctx),
        Cmd::Harness { sub } => cmd_harness(sub, &mut ctx),
        Cmd::Space { sub } => cmd_space(sub, &mut ctx),
        Cmd::Session { sub } => cmd_session(sub, &mut ctx),
        // Legacy top-level spellings. They only reshape their arguments and
        // then re-enter the canonical nested handler, so there is one
        // implementation per operation.
        Cmd::Comment { first, body } => cmd_comment(first, body, &mut ctx),
        Cmd::Done {
            card_id,
            outcome,
            summary,
        } => cmd_card_run(
            args::RunCmd::Done {
                card_id,
                outcome,
                summary,
            },
            &mut ctx,
        ),
        Cmd::Cancel { card_id } => cmd_card_run(args::RunCmd::Cancel { card_id }, &mut ctx),
        Cmd::Retry { card_id } => cmd_card_run(args::RunCmd::Retry { card_id }, &mut ctx),
        Cmd::Move {
            card_id,
            column,
            destination_board,
        } => cmd_move(&mut ctx, card_id, &column, destination_board.as_deref()),
        Cmd::PaneExited { card_id, run_id } => cmd_pane_exited(card_id, run_id, &mut ctx),
    }
}

fn run_daemon(foreground: bool) -> Result<()> {
    board_daemon::run(foreground).map_err(|error| anyhow!(error))
}

fn cmd_version(json_output: bool) -> Result<()> {
    let daemon_version = match UnixClient::connect(&board_core::paths::socket_path()) {
        Ok(mut client) => client.daemon_status().ok().map(|status| status.version),
        Err(_) => None,
    };
    emit_line(
        &json!({
            "cli_version": env!("CARGO_PKG_VERSION"),
            "daemon_version": daemon_version,
        }),
        json_output,
        format!(
            "cli {}\ndaemon {}",
            env!("CARGO_PKG_VERSION"),
            daemon_version.as_deref().unwrap_or("unavailable")
        ),
    )
}

fn print_skill() -> Result<()> {
    std::io::stdout().write_all(include_bytes!("../../../skill/SKILL.md"))?;
    Ok(())
}

/// `--json` among the arguments clap would still treat as options: everything
/// up to a `--` separator. Only consulted when parsing failed outright.
fn json_flag_in_options() -> bool {
    std::env::args()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json")
}

/// The process exit code for a failed command.
///
/// An RPC error reports the daemon's protocol code (`1` bad request, `2` not
/// found, `3` invalid state, `4` herdr unavailable, `5` internal) so scripts can
/// branch on `$?`; any other protocol code is clamped rather than passed
/// through. Errors raised by the CLI itself use [`CLI_ERROR_CODE`].
fn exit_code(error: &anyhow::Error) -> i32 {
    match rpc_error(error) {
        Some(rpc) => match rpc.code {
            code @ 1..=5 => code,
            _ => UNMAPPED_RPC_CODE,
        },
        None => CLI_ERROR_CODE,
    }
}

fn rpc_error(error: &anyhow::Error) -> Option<&RpcClientError> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<RpcClientError>())
}

fn render_error(error: &anyhow::Error, json_requested: bool) {
    if !json_requested {
        eprintln!("board: {error:#}");
        return;
    }

    let payload = if let Some(rpc) = rpc_error(error) {
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
                "code": CLI_ERROR_CODE,
                "kind": "cli",
                "message": format!("{error:#}"),
            }
        })
    };
    eprintln!(
        "{}",
        serde_json::to_string(&payload).unwrap_or_else(|_| format!(
            r#"{{"error":{{"code":{CLI_ERROR_CODE},"kind":"cli","message":"board command failed"}}}}"#
        ))
    );
}
