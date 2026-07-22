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
use board_core::capability::HarnessCapabilities;
use board_core::client::{BoardClient, UnixClient};
use board_core::harness::DEFAULT_HARNESS;
use board_core::paths;
use board_core::protocol::{
    BoardSnapshot, CardCreateParams, CardMoveParams, Effort, RunOutcome, SessionListResult,
    SpaceKind, SpaceListResult,
};
use board_core::scope::{resolve_scope_path, select_scope_candidate};
use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Parser)]
#[command(name = "board", version, about = "herdr-board kanban for agents")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // clap arg enum; boxing hurts ergonomics
enum Cmd {
    /// Open the kanban TUI (auto-starts the daemon).
    Tui,
    /// Run the daemon in the foreground / background.
    Daemon {
        /// Log to stderr as well as the log file, and stay attached.
        #[arg(long)]
        foreground: bool,
        /// Stop the running daemon (graceful) and exit. The plugin build step
        /// uses this so a reinstall replaces a stopped process instead of a
        /// stale binary the old daemon still has mapped in memory.
        #[arg(long)]
        stop: bool,
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
    ///
    /// `ok` with no on_success column marks the card `done` (with a target it
    /// moves instead). If the agent reports done or goes idle without this
    /// command, the card becomes `awaiting`; timeout and pane exit still fail.
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
    #[command(name = "__pane-exited", hide = true)]
    PaneExited {
        /// Card id; defaults to $BOARD_CARD_ID.
        card_id: Option<i64>,
        #[arg(long)]
        run_id: i64,
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
    /// Harness capability queries.
    Harness {
        #[command(subcommand)]
        sub: HarnessCmd,
    },
    /// Run-space (herdr workspace) operations.
    Space {
        #[command(subcommand)]
        sub: SpaceCmd,
    },
    /// herdr session operations.
    Session {
        #[command(subcommand)]
        sub: SessionCmd,
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
        /// herdr session name (default: the daemon's default session).
        #[arg(long)]
        session: Option<String>,
        /// Space kind: `workspace` (open workspace) or `new-workspace`.
        #[arg(long)]
        space_kind: Option<String>,
        #[arg(long)]
        space_ref: Option<String>,
        /// Working directory for a `new-workspace` space (required for that kind).
        #[arg(long)]
        space_cwd: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Archive an idle/done/failed card without deleting its history.
    Archive {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Restore an archived card to the active board.
    Restore {
        id: i64,
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

#[derive(Subcommand)]
enum HarnessCmd {
    /// List every available harness (built-ins `pi`/`claude` + config-defined).
    List {
        #[arg(long)]
        json: bool,
    },
    /// List known models and the efforts each accepts.
    Models {
        /// Harness name.
        #[arg(default_value = DEFAULT_HARNESS)]
        harness: String,
        #[arg(long)]
        json: bool,
    },
    /// Show the efforts a model accepts.
    Efforts {
        /// Harness name.
        #[arg(default_value = DEFAULT_HARNESS)]
        harness: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        json: bool,
    },
    /// List the permission modes a harness understands.
    Permissions {
        /// Harness name.
        #[arg(default_value = DEFAULT_HARNESS)]
        harness: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SpaceCmd {
    /// List run spaces (herdr workspaces) in a session.
    List {
        /// herdr session name (default: the daemon's default session).
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    /// List herdr sessions.
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
        Cmd::Cancel { card_id, json } => cmd_run_action("run.cancel", card_id, json),
        Cmd::Retry { card_id, json } => cmd_run_action("run.retry", card_id, json),
        Cmd::Column { sub } => cmd_column(sub),
        Cmd::Harness { sub } => cmd_harness(sub),
        Cmd::Space { sub } => cmd_space(sub),
        Cmd::Session { sub } => cmd_session(sub),
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

/// `board daemon --stop`: gracefully shut down the running daemon over the
/// socket, then wait for its listener to vanish. Idempotent — if nothing is
/// running (or only a stale socket file remains) it cleans up and succeeds.
/// Used by the plugin build step before a reinstall.
fn stop_daemon() -> Result<()> {
    let path = paths::socket_path();

    let mut client = match UnixClient::connect(&path) {
        Ok(c) => c,
        Err(_) => {
            // No live listener: clear any stale socket file left by a crash.
            let _ = std::fs::remove_file(&path);
            println!("boardd not running");
            return Ok(());
        }
    };

    // Ask the daemon to shut itself down. A daemon older than `daemon.stop`
    // rejects it; tell the user to stop it manually in that case.
    let stop_result = client.daemon_stop();
    drop(client);
    if let Err(e) = stop_result {
        let _ = std::fs::remove_file(&path);
        bail!(
            "could not stop boardd gracefully ({e}); it may predate `daemon.stop` — \
             stop it manually, e.g. `pkill -f 'board daemon'`"
        );
    }

    // Wait for the listener to disappear (the daemon removes the socket on
    // exit), so the next launch spawns a fresh process rather than racing a
    // half-dead one still holding the single-instance lock.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut delay = Duration::from_millis(25);
    while Instant::now() < deadline {
        if UnixClient::connect(&path).is_err() {
            break;
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(Duration::from_millis(200));
    }
    let _ = std::fs::remove_file(&path);
    println!("boardd stopped");
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
            session,
            space_kind,
            space_ref,
            space_cwd,
            json,
        } => {
            let board = open_current_board(&mut c)?;
            let column_id = match column {
                Some(s) => Some(resolve_column_in(&board, &s)?),
                None => None,
            };
            let effort = match effort {
                Some(s) => {
                    Some(Effort::parse_str(&s).ok_or_else(|| anyhow!("invalid effort: {s}"))?)
                }
                None => None,
            };
            let space_kind = match space_kind {
                Some(s) => Some(parse_space_kind(&s)?),
                None => None,
            };
            let p = CardCreateParams {
                title,
                board_id: Some(board.board.id),
                description,
                column_id,
                harness,
                model,
                effort,
                permission_mode: permission,
                session,
                space_kind,
                space_ref,
                space_cwd,
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
        CardCmd::Archive { id, json } => card_archive(&mut c, id, true, json)?,
        CardCmd::Restore { id, json } => card_archive(&mut c, id, false, json)?,
        CardCmd::Show { id, json } => {
            let d = c.card_get(id)?;
            if json {
                print_json(&d)?;
            } else {
                println!(
                    "#{} {}  [{}{}]",
                    d.card.id,
                    d.card.title,
                    d.card.status,
                    if d.card.archived_at.is_some() {
                        ", archived"
                    } else {
                        ""
                    }
                );
                if let Some(session) = &d.card.session {
                    println!("session: {session}");
                }
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
            let board = open_current_board(&mut c)?;
            let column_id = match column {
                Some(s) => Some(resolve_column_in(&board, &s)?),
                None => None,
            };
            let cards = c.card_list_for_board(Some(board.board.id), column_id)?;
            if json {
                print_json(&cards)?;
            } else {
                for card in &cards {
                    let session = card
                        .session
                        .as_deref()
                        .map(|s| format!("\tsession={s}"))
                        .unwrap_or_default();
                    let archived = if card.archived_at.is_some() {
                        "\tarchived"
                    } else {
                        ""
                    };
                    println!(
                        "#{}\t[{}]\tcol={}\t{}{}{}",
                        card.id, card.status, card.column_id, card.title, session, archived
                    );
                }
            }
        }
    }
    Ok(())
}

fn card_archive(c: &mut UnixClient, id: i64, archived: bool, json: bool) -> Result<()> {
    let card = c.card_archive(id, archived)?;
    if json {
        print_json(&card)?;
    } else if archived {
        println!("Archived card #{}", card.id);
    } else {
        println!("Restored card #{}", card.id);
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
    let run_id = match std::env::var("BOARD_RUN_ID") {
        Ok(value) => Some(
            value
                .parse::<i64>()
                .map_err(|_| anyhow!("invalid $BOARD_RUN_ID '{value}': expected an integer"))?,
        ),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(_)) => {
            bail!("invalid $BOARD_RUN_ID: value is not valid UTF-8")
        }
    };
    let mut c = connect_or_start()?;
    let res = c.run_done_for_run(card_id, outcome, summary.as_deref(), run_id)?;
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

fn cmd_pane_exited(card_id: Option<i64>, run_id: i64) -> Result<()> {
    let card_id = match card_id {
        Some(id) => id,
        None => env_card_id()?,
    };
    connect_or_start()?.run_pane_exited(card_id, run_id)?;
    Ok(())
}

fn cmd_move(card_id: i64, column: String, json: bool) -> Result<()> {
    let mut c = connect_or_start()?;
    let card = c.card_get(card_id)?.card;
    let board = c.board_get_by_id(card.board_id)?;
    let column_id = resolve_column_in(&board, &column)?;
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
            let snap = open_current_board(&mut c)?;
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

fn cmd_harness(sub: HarnessCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        HarnessCmd::List { json } => {
            let v = c.call("harness.list", json!({}))?;
            let names: Vec<String> = serde_json::from_value(v["harnesses"].clone())?;
            if json {
                print_json(&names)?;
            } else {
                for h in &names {
                    println!("{h}");
                }
            }
        }
        HarnessCmd::Models { harness, json } => {
            let caps = harness_capabilities(&mut c, &harness)?;
            if json {
                print_json(&caps)?;
            } else {
                for m in &caps.models {
                    println!("{}  {}", m.id, efforts_str(&m.efforts));
                }
                if caps.model_freeform {
                    if caps.models.is_empty() {
                        println!("(any model string accepted; catalog comes from harness config)");
                    } else {
                        println!("\n(any model string accepted; these are known aliases)");
                    }
                }
            }
        }
        HarnessCmd::Efforts {
            harness,
            model,
            json,
        } => {
            let caps = harness_capabilities(&mut c, &harness)?;
            let (efforts, known) = match caps.models.iter().find(|m| m.id == model) {
                Some(m) => (m.efforts.clone(), true),
                None if caps.model_freeform => (union_efforts(&caps), false),
                None => bail!("model '{model}' not known to harness '{harness}'"),
            };
            if json {
                let efforts: Vec<&str> = efforts.iter().map(|e| e.as_str()).collect();
                print_json(&json!({ "model": model, "efforts": efforts, "known": known }))?;
            } else {
                println!("{}", efforts_str(&efforts));
                if !known {
                    println!(
                        "\n(model '{model}' unknown to {harness} but accepted; \
                         showing all known efforts)"
                    );
                }
            }
        }
        HarnessCmd::Permissions { harness, json } => {
            let caps = harness_capabilities(&mut c, &harness)?;
            if json {
                print_json(&caps.permission_modes)?;
            } else {
                for p in &caps.permission_modes {
                    println!("{p}");
                }
            }
        }
    }
    Ok(())
}

fn cmd_space(sub: SpaceCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        SpaceCmd::List { session, json } => {
            let v = c.call("space.list", json!({ "session": session }))?;
            let res: SpaceListResult = serde_json::from_value(v)?;
            if json {
                print_json(&res)?;
            } else {
                let width = res.spaces.iter().map(|s| s.id.len()).max().unwrap_or(0);
                for s in &res.spaces {
                    println!("{:<width$}  {}", s.id, s.label);
                }
            }
        }
    }
    Ok(())
}

fn cmd_session(sub: SessionCmd) -> Result<()> {
    let mut c = connect_or_start()?;
    match sub {
        SessionCmd::List { json } => {
            let v = c.call("session.list", json!({}))?;
            let res: SessionListResult = serde_json::from_value(v)?;
            if json {
                print_json(&res)?;
            } else {
                let width = res.sessions.iter().map(|s| s.name.len()).max().unwrap_or(0);
                for s in &res.sessions {
                    let running = if s.running { "running" } else { "stopped" };
                    let marker = if s.default { "  (default)" } else { "" };
                    println!("{:<width$}  {:<8}{}", s.name, running, marker);
                }
            }
        }
    }
    Ok(())
}

/// Parse a `--space-kind` CLI value. Accepts `workspace` and `new-workspace`
/// (the wire form `new_workspace` is also tolerated); anything else is an error.
fn parse_space_kind(s: &str) -> Result<SpaceKind> {
    match s {
        "workspace" => Ok(SpaceKind::Workspace),
        "new-workspace" | "new_workspace" => Ok(SpaceKind::NewWorkspace),
        other => bail!("invalid space-kind '{other}' (expected: workspace, new-workspace)"),
    }
}

// -- helpers -----------------------------------------------------------------

/// Fetch a harness's capability catalog (`harness.capabilities`).
fn harness_capabilities(c: &mut UnixClient, harness: &str) -> Result<HarnessCapabilities> {
    let v = c.call("harness.capabilities", json!({ "harness": harness }))?;
    Ok(serde_json::from_value(v)?)
}

/// Render an effort list space-separated (e.g. `low medium high xhigh max`).
fn efforts_str(efforts: &[Effort]) -> String {
    efforts
        .iter()
        .map(|e| e.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Deduplicated default/free-form efforts followed by every model's efforts,
/// preserving first-seen order.
fn union_efforts(caps: &HarnessCapabilities) -> Vec<Effort> {
    let mut out = caps.default_efforts.clone();
    for m in &caps.models {
        for e in &m.efforts {
            if !out.contains(e) {
                out.push(*e);
            }
        }
    }
    out
}

fn env_card_id() -> Result<i64> {
    std::env::var("BOARD_CARD_ID")
        .ok()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| anyhow!("no card id given and $BOARD_CARD_ID is unset"))
}

fn current_scope_path() -> Result<String> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let override_path = std::env::var("BOARD_SCOPE_PATH").ok();
    let plugin_context = std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok();
    let candidate =
        select_scope_candidate(override_path.as_deref(), plugin_context.as_deref(), &cwd)?;
    let resolved = resolve_scope_path(&candidate)?;
    resolved.to_str().map(str::to_string).ok_or_else(|| {
        anyhow!(
            "board scope path is not valid UTF-8: {}",
            resolved.display()
        )
    })
}

fn open_current_board(c: &mut UnixClient) -> Result<BoardSnapshot> {
    c.board_open(&current_scope_path()?)
}

/// Resolve a column reference within one board snapshot.
fn resolve_column_in(snap: &BoardSnapshot, s: &str) -> Result<i64> {
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
