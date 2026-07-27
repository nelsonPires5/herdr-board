use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "board", version, about = "herdr-board kanban for agents")]
pub(crate) struct Cli {
    /// Select a board by stable id or canonical scope path.
    #[arg(long, global = true, value_name = "ID|PATH")]
    pub(crate) board: Option<String>,
    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Cmd {
    /// Open the kanban TUI (auto-starts the daemon).
    Tui,
    /// Run or inspect the daemon.
    Daemon {
        /// Log to stderr as well as the log file, and stay attached.
        #[arg(long)]
        foreground: bool,
        /// Stop the running daemon (graceful).
        #[arg(long)]
        stop: bool,
        #[command(subcommand)]
        sub: Option<DaemonCmd>,
    },
    /// Report CLI and daemon versions without starting the daemon.
    Version {
        #[arg(long)]
        json: bool,
    },
    /// Print the exact operational skill document.
    Skill,
    /// Board operations.
    Board {
        #[command(subcommand)]
        sub: BoardCmd,
    },
    /// Apply a board template.
    Template {
        #[command(subcommand)]
        sub: TemplateCmd,
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
    #[command(name = "__pane-exited", hide = true)]
    PaneExited {
        /// Card id; defaults to $BOARD_CARD_ID.
        card_id: Option<i64>,
        #[arg(long)]
        run_id: i64,
    },
    /// Move a card to a column (name, case-insensitive, or id).
    Move {
        card_id: i64,
        column: String,
        /// Destination board for a cross-board move. The global --board is also accepted.
        #[arg(long, alias = "to-board", value_name = "ID|PATH")]
        destination_board: Option<String>,
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
pub(crate) enum DaemonCmd {
    /// Show operational daemon status.
    Status {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum BoardCmd {
    /// List all boards.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one board by id, path, or the selected/current board.
    Show {
        selector: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Open (or create) the board for a scope path.
    Open {
        path: String,
        #[arg(long)]
        json: bool,
    },
    /// Rename a board by id, path, or the selected/current board.
    Rename {
        /// `<ID|PATH> <NAME>` or `<NAME>` with global --board/current scope.
        #[arg(required = true, num_args = 1..=2)]
        values: Vec<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum TemplateCmd {
    /// Apply a named template to the selected board.
    Apply {
        name: String,
        #[arg(long)]
        json: bool,
    },
}

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
        #[arg(long)]
        json: bool,
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
        #[arg(long)]
        json: bool,
    },
    /// Permanently delete a card and its history.
    Delete {
        id: i64,
        #[arg(long)]
        yes: bool,
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
    /// List cards (optionally filtered by column and visibility).
    List {
        #[arg(long)]
        column: Option<String>,
        #[arg(long, value_parser = ["active", "all", "archived"])]
        visibility: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Move a card, optionally across boards.
    Move {
        id: i64,
        column: String,
        #[arg(long, alias = "to-board", value_name = "ID|PATH")]
        destination_board: Option<String>,
        #[arg(long)]
        json: bool,
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
    Add {
        card_id: i64,
        body: String,
        #[arg(long)]
        json: bool,
    },
    /// Show a comment by id.
    Show {
        comment_id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Edit a comment body.
    Edit {
        comment_id: i64,
        body: String,
        #[arg(long)]
        json: bool,
    },
    /// Soft-delete a comment.
    Delete {
        comment_id: i64,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show a comment's audit history.
    History {
        comment_id: i64,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum RunCmd {
    /// Close the active run.
    Done {
        card_id: Option<i64>,
        #[arg(long, value_parser = ["ok", "fail"])]
        outcome: String,
        #[arg(long)]
        summary: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Cancel the active run.
    Cancel {
        card_id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Retry the card in its current column.
    Retry {
        card_id: i64,
        #[arg(long)]
        json: bool,
    },
    /// Focus one exact run's pane (the run id is required).
    Focus {
        card_id: i64,
        run_id: i64,
        #[arg(long)]
        origin_socket: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Confirm an awaiting run (same completion channel as `run done --outcome ok`).
    Confirm {
        card_id: Option<i64>,
        #[arg(long)]
        summary: Option<String>,
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
        #[arg(long)]
        fresh_session: bool,
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
        #[arg(long)]
        json: bool,
    },
    /// Show a column by id or name.
    Show {
        column: String,
        #[arg(long)]
        json: bool,
    },
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
        #[arg(long)]
        fresh_session: bool,
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
        #[arg(long)]
        json: bool,
    },
    /// Reorder a column by its zero-based position.
    Reorder {
        column: String,
        position: i64,
        #[arg(long)]
        json: bool,
    },
    /// Delete a column.
    Delete {
        column: String,
        #[arg(long)]
        move_cards_to: Option<String>,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum HarnessCmd {
    /// List every available harness.
    List {
        #[arg(long)]
        json: bool,
    },
    /// List known models and the efforts each accepts.
    Models {
        #[arg(default_value = "pi")]
        harness: String,
        #[arg(long)]
        json: bool,
    },
    /// Show the efforts a model accepts.
    Efforts {
        #[arg(default_value = "pi")]
        harness: String,
        #[arg(long)]
        model: String,
        #[arg(long)]
        json: bool,
    },
    /// List permission modes a harness understands.
    Permissions {
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
