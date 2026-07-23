use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "board", version, about = "herdr-board kanban for agents")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)] // clap arg enum; boxing hurts ergonomics
pub(crate) enum Cmd {
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
pub(crate) enum CardCmd {
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
pub(crate) enum ColumnCmd {
    /// List columns.
    List {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum HarnessCmd {
    /// List every available harness (built-ins `pi`/`claude` + config-defined).
    List {
        #[arg(long)]
        json: bool,
    },
    /// List known models and the efforts each accepts.
    Models {
        /// Harness name.
        #[arg(default_value = "pi")]
        harness: String,
        #[arg(long)]
        json: bool,
    },
    /// Show the efforts a model accepts.
    Efforts {
        /// Harness name.
        #[arg(default_value = "pi")]
        harness: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        json: bool,
    },
    /// List the permission modes a harness understands.
    Permissions {
        /// Harness name.
        #[arg(default_value = "pi")]
        harness: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum SpaceCmd {
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
pub(crate) enum SessionCmd {
    /// List herdr sessions.
    List {
        #[arg(long)]
        json: bool,
    },
}
