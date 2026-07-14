//! board — the single CLI binary (OWNED BY PHASE D).
//!
//! clap subcommands over the boardd socket: `tui`, `daemon`, `status`, and the
//! agent/user verbs (`card`, `comment`, `done`, `move`, `cancel`, `retry`,
//! `column`). Every connecting command auto-starts the daemon if it is absent.

use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use board_core::client::{BoardClient, UnixClient};
use board_core::paths;
use board_core::protocol::{CardCreateParams, CardMoveParams, Effort, RunOutcome, SpaceKind};
use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Parser)]
#[command(name = "board", version, about = "herdr-board kanban for agents")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open the kanban TUI (auto-starts the daemon).
    Tui,
    /// Run the daemon in the foreground / background.
    Daemon {
        /// Log to stderr as well as the log file, and stay attached.
        #[arg(long)]
        foreground: bool,
    },
    /// Show daemon status.
    Status {
        #[arg(long)]
        json: bool,
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
        #[arg(long)]
        json: bool,
    },
    /// Close the active run (`board done [CARD_ID] --outcome ok|fail`).
    Done {
        /// Card id; defaults to $BOARD_CARD_ID.
        card_id: Option<i64>,
        #[arg(long, value_parser = ["ok", "fail"])]
        outcome: String,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Move a card to a column (name — case-insensitive — or id).
    Move {
        card_id: i64,
        column: String,
        #[arg(long)]
        json: bool,
    },
    /// Cancel a card's run.
    Cancel {
        card_id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Retry a card (new forked run in its current column).
    Retry {
        card_id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Column operations.
    Column {
        #[command(subcommand)]
        sub: ColumnCmd,
    },
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // clap arg enum; boxing hurts ergonomics
enum CardCmd {
    /// Create a card.
    New {
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
        #[arg(long)]
        space_kind: Option<String>,
        #[arg(long)]
        space_ref: Option<String>,
        #[arg(long)]
        worktree_base: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show a card with comments and run history.
    Show {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    /// List cards (optionally filtered by column name/id).
    List {
        #[arg(long)]
        column: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum ColumnCmd {
    /// List columns.
    List {
        #[arg(long)]
        json: bool,
    },
}

fn main() {
    if let Err(e) = real_main() {
        eprintln!("board: {e:#}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Daemon { foreground } => board_daemon::run(foreground).map_err(|e| anyhow!(e)),
        Cmd::Tui => {
            let client = connect_or_start()?;
            board_tui::run(Box::new(client))
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
        Cmd::Move {
            card_id,
            column,
            json,
        } => cmd_move(card_id, column, json),
        Cmd::Cancel { card_id, json } => cmd_run_action("run.cancel", card_id, json),
        Cmd::Retry { card_id, json } => cmd_run_action("run.retry", card_id, json),
        Cmd::Column { sub } => cmd_column(sub),
    }
}

// -- connection / auto-start -------------------------------------------------

fn connect_or_start() -> Result<UnixClient> {
    let path = paths::socket_path();
    if let Ok(c) = UnixClient::connect(&path) {
        return Ok(c);
    }
    spawn_daemon().context("auto-starting boardd")?;

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut delay = Duration::from_millis(50);
    loop {
        std::thread::sleep(delay);
        if let Ok(c) = UnixClient::connect(&path) {
            return Ok(c);
        }
        if Instant::now() >= deadline {
            bail!("could not connect to boardd at {}", path.display());
        }
        delay = (delay * 2).min(Duration::from_millis(500));
    }
}

fn spawn_daemon() -> Result<()> {
    let exe = std::env::current_exe()?;
    let log_path = paths::log_path();
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let err = out.try_clone()?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    // Detach into a new process group so it outlives this CLI invocation.
    cmd.process_group(0);
    cmd.spawn()?;
    Ok(())
}

// -- command bodies ----------------------------------------------------------

fn cmd_status(json: bool) -> Result<()> {
    let mut c = connect_or_start()?;
    let s = c.daemon_status()?;
    if json {
        print_json(&s)?;
    } else {
        println!(
            "boardd {}  db={}  herdr={}  active={}  queued={}",
            s.version,
            s.db_path,
            if s.herdr_connected {
                "connected"
            } else {
                "absent"
            },
            s.active_runs,
            s.queued_runs
        );
    }
    Ok(())
}

fn cmd_card(sub: CardCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        CardCmd::New {
            title,
            description,
            column,
            harness,
            model,
            effort,
            permission,
            space_kind,
            space_ref,
            worktree_base,
            json,
        } => {
            let column_id = match column {
                Some(s) => Some(resolve_column(&mut c, &s)?),
                None => None,
            };
            let effort = match effort {
                Some(s) => {
                    Some(Effort::parse_str(&s).ok_or_else(|| anyhow!("invalid effort: {s}"))?)
                }
                None => None,
            };
            let space_kind = match space_kind {
                Some(s) => Some(
                    SpaceKind::parse_str(&s).ok_or_else(|| anyhow!("invalid space-kind: {s}"))?,
                ),
                None => None,
            };
            let p = CardCreateParams {
                title,
                description,
                column_id,
                harness,
                model,
                effort,
                permission_mode: permission,
                space_kind,
                space_ref,
                worktree_base,
                position: None,
            };
            let card = c.card_create(&p)?;
            if json {
                print_json(&card)?;
            } else {
                println!(
                    "Created card #{} \"{}\" in column {}",
                    card.id, card.title, card.column_id
                );
            }
        }
        CardCmd::Show { id, json } => {
            let d = c.card_get(id)?;
            if json {
                print_json(&d)?;
            } else {
                println!("#{} {}  [{}]", d.card.id, d.card.title, d.card.status);
                if !d.card.description.is_empty() {
                    println!("\n{}", d.card.description);
                }
                if !d.comments.is_empty() {
                    println!("\nComments:");
                    for cm in &d.comments {
                        println!(
                            "  [{}] {} ({}): {}",
                            cm.id, cm.author, cm.created_at, cm.body
                        );
                    }
                }
                if !d.runs.is_empty() {
                    println!("\nRuns:");
                    for r in &d.runs {
                        println!(
                            "  #{} col={} {} started={:?} ended={:?}",
                            r.id,
                            r.column_id,
                            r.outcome
                                .map(|o| o.to_string())
                                .unwrap_or_else(|| "-".into()),
                            r.started_at,
                            r.ended_at
                        );
                    }
                }
            }
        }
        CardCmd::List { column, json } => {
            let column_id = match column {
                Some(s) => Some(resolve_column(&mut c, &s)?),
                None => None,
            };
            let cards = c.card_list(column_id)?;
            if json {
                print_json(&cards)?;
            } else {
                for card in &cards {
                    println!(
                        "#{}\t[{}]\tcol={}\t{}",
                        card.id, card.status, card.column_id, card.title
                    );
                }
            }
        }
    }
    Ok(())
}

fn cmd_comment(first: String, body: Option<String>, json: bool) -> Result<()> {
    let (card_id, body) = match body {
        Some(b) => (first.parse::<i64>().context("card id")?, b),
        None => (env_card_id()?, first),
    };
    let author = std::env::var("BOARD_RUN_ID")
        .ok()
        .map(|r| format!("agent:{r}"));
    let mut c = connect_or_start()?;
    let comment = c.comment_add(card_id, &body, author.as_deref())?;
    if json {
        print_json(&comment)?;
    } else {
        println!("Commented on card #{card_id} (comment #{})", comment.id);
    }
    Ok(())
}

fn cmd_done(
    card_id: Option<i64>,
    outcome: String,
    summary: Option<String>,
    json: bool,
) -> Result<()> {
    let card_id = match card_id {
        Some(id) => id,
        None => env_card_id()?,
    };
    let outcome = RunOutcome::parse_str(&outcome).ok_or_else(|| anyhow!("invalid outcome"))?;
    let mut c = connect_or_start()?;
    let res = c.run_done(card_id, outcome, summary.as_deref())?;
    if json {
        print_json(&res)?;
    } else {
        println!(
            "Run #{} closed ({}); card #{} now [{}] in column {}",
            res.run.id, outcome, res.card.id, res.card.status, res.card.column_id
        );
    }
    Ok(())
}

fn cmd_move(card_id: i64, column: String, json: bool) -> Result<()> {
    let mut c = connect_or_start()?;
    let column_id = resolve_column(&mut c, &column)?;
    let card = c.card_move(&CardMoveParams {
        id: card_id,
        column_id,
        position: None,
    })?;
    if json {
        print_json(&card)?;
    } else {
        println!(
            "Moved card #{} to column {} [{}]",
            card.id, card.column_id, card.status
        );
    }
    Ok(())
}

fn cmd_run_action(method: &str, card_id: i64, json: bool) -> Result<()> {
    let mut c = connect_or_start()?;
    let v = c.call(method, json!({ "card_id": card_id }))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let action = if method == "run.cancel" {
            "Cancelled"
        } else {
            "Retried"
        };
        println!("{action} card #{card_id}");
    }
    Ok(())
}

fn cmd_column(sub: ColumnCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        ColumnCmd::List { json } => {
            let snap = c.board_get()?;
            if json {
                print_json(&snap.columns)?;
            } else {
                for col in &snap.columns {
                    println!(
                        "#{}\tpos={}\t[{}]\t{}",
                        col.id, col.position, col.trigger, col.name
                    );
                }
            }
        }
    }
    Ok(())
}

// -- helpers -----------------------------------------------------------------

fn env_card_id() -> Result<i64> {
    std::env::var("BOARD_CARD_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("no card id given and $BOARD_CARD_ID is unset"))
}

/// Resolve a column reference (numeric id or case-insensitive name) to its id.
fn resolve_column(c: &mut UnixClient, s: &str) -> Result<i64> {
    let snap = c.board_get()?;
    if let Ok(id) = s.parse::<i64>() {
        if snap.columns.iter().any(|col| col.id == id) {
            return Ok(id);
        }
    }
    let lower = s.to_lowercase();
    snap.columns
        .iter()
        .find(|col| col.name.to_lowercase() == lower)
        .map(|col| col.id)
        .ok_or_else(|| anyhow!("no column matching \"{s}\""))
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(v)?);
    Ok(())
}
