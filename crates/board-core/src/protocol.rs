//! boardd socket protocol (v1) — serde types, the single source of truth.
//!
//! See `docs/protocol.md` for semantics. Transport is newline-delimited JSON over a
//! Unix socket; every request/response/event and every method's params/result is
//! represented here.

use serde::{
    de::{self, Visitor},
    Deserialize, Deserializer, Serialize, Serializer,
};

use crate::model::{Board, Card, Column, Comment, CommentHistory, CommentRecord, Run};

/// A nullable field in a partial update.
///
/// `Unchanged` is represented by an omitted JSON member, `Clear` by JSON
/// `null`, and `Set` by the value itself. This keeps protocol v1 compatible
/// while allowing clients to intentionally clear a stored nullable value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Patch<T> {
    #[default]
    Unchanged,
    Clear,
    Set(T),
}

impl<T> Patch<T> {
    pub fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }
}

impl<T: Serialize> Serialize for Patch<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            // Update fields use `skip_serializing_if` to omit this variant.
            // Serializing it directly as null is the least surprising fallback.
            Self::Unchanged | Self::Clear => serializer.serialize_none(),
            Self::Set(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PatchVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for PatchVisitor<T> {
            type Value = Patch<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("null or a patch value")
            }

            fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Patch::Clear)
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Patch::Clear)
            }

            fn visit_some<D: Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Self::Value, D::Error> {
                T::deserialize(deserializer).map(Patch::Set)
            }
        }

        deserializer.deserialize_option(PatchVisitor(std::marker::PhantomData))
    }
}

// ---------------------------------------------------------------------------
// Shared enums
// ---------------------------------------------------------------------------

/// Column trigger: `auto` starts a run on entry, `manual` waits for a human.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    Manual,
    Auto,
}

/// Where a card's agent runs, within its herdr session.
///
/// - [`SpaceKind::Workspace`] — an ALREADY-OPEN workspace in the session;
///   `space_ref` is its workspace id (or, on dispatch, a case-insensitive label).
/// - [`SpaceKind::NewWorkspace`] — the daemon creates a workspace on first
///   dispatch (label = `space_ref`, cwd = `space_cwd`), reusing an existing
///   workspace with that label if one is already open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceKind {
    Workspace,
    NewWorkspace,
}

/// Live card status.
///
/// - [`CardStatus::Awaiting`] — the agent finished (or went idle past the
///   grace period) without an explicit `board done`; the run stays OPEN and
///   the column timeout is paused. Never becomes a failure on its own.
/// - [`CardStatus::Done`] — completion confirmed via `board done ok` with no
///   target column (with a target column the card moves instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardStatus {
    Idle,
    Queued,
    Running,
    Blocked,
    Failed,
    Awaiting,
    Done,
}

/// Why a card entered [`CardStatus::Awaiting`]. Set on entry, cleared (NULL)
/// on exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AwaitingReason {
    /// herdr reported `agent_status=done` and no `board done` arrived.
    AgentDone,
    /// `agent_status=idle` sustained past `idle_grace_seconds`, no `board done`.
    IdleExpired,
}

/// Terminal outcome of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunOutcome {
    Ok,
    Fail,
    Cancelled,
    Lost,
}

/// Which archived-card set a card list should expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardVisibility {
    Active,
    All,
    Archived,
}

/// Reasoning effort level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

macro_rules! str_enum {
    ($ty:ty { $($variant:ident => $s:literal),+ $(,)? }) => {
        impl $ty {
            /// Canonical wire/DB string.
            pub fn as_str(&self) -> &'static str {
                match self { $( <$ty>::$variant => $s ),+ }
            }
            /// Parse from a wire/DB string.
            pub fn parse_str(s: &str) -> Option<Self> {
                match s { $( $s => Some(<$ty>::$variant), )+ _ => None }
            }
        }
        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

str_enum!(Trigger { Manual => "manual", Auto => "auto" });
str_enum!(SpaceKind { Workspace => "workspace", NewWorkspace => "new_workspace" });
str_enum!(CardStatus {
    Idle => "idle", Queued => "queued", Running => "running",
    Blocked => "blocked", Failed => "failed",
    Awaiting => "awaiting", Done => "done",
});
str_enum!(AwaitingReason {
    AgentDone => "agent_done", IdleExpired => "idle_expired",
});
str_enum!(RunOutcome {
    Ok => "ok", Fail => "fail", Cancelled => "cancelled", Lost => "lost",
});
str_enum!(CardVisibility {
    Active => "active", All => "all", Archived => "archived",
});
str_enum!(Effort {
    Off => "off", Minimal => "minimal", Low => "low", Medium => "medium",
    High => "high", Xhigh => "xhigh", Max => "max",
});

// ---------------------------------------------------------------------------
// Envelope
// ---------------------------------------------------------------------------

/// A request line: `{"id","method","params"?}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A response line: `{"id","result"}` or `{"id","error"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: impl Into<String>, result: serde_json::Value) -> Self {
        Response {
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }
    pub fn err(id: impl Into<String>, code: i32, message: impl Into<String>) -> Self {
        Self::err_with_details(id, code, None::<String>, message, None)
    }

    /// Construct an additive structured error without changing the legacy
    /// `Response::err` call sites.
    pub fn err_with_details(
        id: impl Into<String>,
        code: i32,
        kind: Option<impl Into<String>>,
        message: impl Into<String>,
        details: Option<serde_json::Value>,
    ) -> Self {
        Response {
            id: id.into(),
            result: None,
            error: Some(RpcError {
                code,
                kind: kind.map(Into::into),
                message: message.into(),
                details,
            }),
        }
    }
}

/// Structured error payload. `kind` and `details` are optional additions so
/// protocol-v1 clients that only know `code` and `message` remain readable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Why the board changed (coarse; clients refetch `board.get`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardChangedReason {
    CardMoved,
    CardCreated,
    CardUpdated,
    CardDeleted,
    CardArchived,
    ColumnChanged,
    CommentAdded,
    RunStarted,
    RunEnded,
    RunBlocked,
}

/// Streamed to subscribers (no `id` field on the wire).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    BoardChanged {
        reason: BoardChangedReason,
        /// The board the change happened on. `None` = a coarse, board-agnostic
        /// refresh signal (subscribers refetch whatever boards they hold).
        /// A cross-board card transfer emits one event per affected board.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        board_id: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        card_id: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column_id: Option<i64>,
    },
    RunEnded {
        card_id: i64,
        run_id: i64,
        outcome: RunOutcome,
    },
}

// ---------------------------------------------------------------------------
// daemon methods
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub version: String,
    pub db_path: String,
    pub herdr_connected: bool,
    pub active_runs: i64,
    pub queued_runs: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StopResult {
    pub stopping: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscribeResult {
    pub subscribed: bool,
}

// ---------------------------------------------------------------------------
// board / column methods
// ---------------------------------------------------------------------------

/// `board.open` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardOpenParams {
    pub scope_path: String,
}

/// `board.rename` params. The board id is stable; only its display name is
/// changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardRenameParams {
    #[serde(alias = "id")]
    pub board_id: i64,
    pub name: String,
}

/// `board.get` params. Omitted id preserves the legacy Global default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardGetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
}

/// `board.list` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardListResult {
    pub boards: Vec<Board>,
}

/// A compact view of a run that is currently started and open on a board.
///
/// This is intentionally separate from [`Run`]: board snapshots only need the
/// card identity and start point required by clients to render live-run state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveRunSummary {
    pub card_id: i64,
    pub started_at: String,
}

/// `board.get` / `board.open` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub board: Board,
    pub columns: Vec<Column>,
    pub cards: Vec<Card>,
    /// Started, open runs for cards belonging to this board. The default keeps
    /// older v1 clients/snapshots readable when this additive field is absent.
    #[serde(default)]
    pub active_runs: Vec<ActiveRunSummary>,
}

/// `column.create` params.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnCreateParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_success_column_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_fail_column_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_session: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<i64>,
}

/// `column.update` params — any subset; `id` required.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ColumnUpdateParams {
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub system_prompt: Patch<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger: Option<Trigger>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub on_success_column_id: Patch<i64>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub on_fail_column_id: Patch<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_session: Option<bool>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub harness_override: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub model_override: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub effort_override: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub permission_override: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub timeout_minutes: Patch<i64>,
}

/// `column.reorder` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnReorderParams {
    pub id: i64,
    pub position: i64,
}

/// `column.delete` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnDeleteParams {
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_cards_to: Option<i64>,
}

/// `{deleted:true}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeletedResult {
    pub deleted: bool,
}

/// `template.apply` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemplateApplyParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
}

// ---------------------------------------------------------------------------
// card methods
// ---------------------------------------------------------------------------

/// `card.create` params.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CardCreateParams {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// herdr session name; `None` = the daemon's default session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_kind: Option<SpaceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_ref: Option<String>,
    /// Working directory for a `new_workspace` space (required for that kind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
}

/// `card.update` params — any subset; `id` required.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CardUpdateParams {
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub model: Patch<String>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub effort: Patch<Effort>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub permission_mode: Patch<String>,
    /// herdr session name; `null` clears the card's explicit session.
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub session: Patch<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_kind: Option<SpaceKind>,
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub space_ref: Patch<String>,
    /// Working directory for a `new_workspace` space.
    #[serde(default, skip_serializing_if = "Patch::is_unchanged")]
    pub space_cwd: Patch<String>,
}

/// `card.archive` params — archive (`true`) or restore (`false`) a card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardArchiveParams {
    pub id: i64,
    pub archived: bool,
}

/// `card.move` params — the dispatch trigger.
///
/// `board_id` declares the destination board for a cross-board transfer:
/// when present and different from the card's current board, the card is
/// transferred (its `cards.board_id`/`column_id` are moved atomically).
/// Omitted (or equal to the current board) keeps the historical intra-board
/// move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardMoveParams {
    pub id: i64,
    pub column_id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
}

/// `card.get` / `card.delete` / etc. by-id params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardIdParams {
    pub id: i64,
}

/// `card.list` params.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CardListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column_id: Option<i64>,
    /// Omitted preserves the active-only board view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility: Option<CardVisibility>,
}

/// `card.get` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardDetail {
    pub card: Card,
    pub comments: Vec<Comment>,
    pub runs: Vec<Run>,
}

// ---------------------------------------------------------------------------
// comment / run methods
// ---------------------------------------------------------------------------

/// `comment.add` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentAddParams {
    pub card_id: i64,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Optional daemon-supplied actor identity used to authorize agent-owned
    /// comments. It is additive and absent for human callers.
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "run_id")]
    pub actor_run_id: Option<i64>,
}

/// `comment.get`, `comment.delete`, and `comment.history` id params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentIdParams {
    #[serde(alias = "comment_id")]
    pub id: i64,
}

pub type CommentGetParams = CommentIdParams;
pub type CommentHistoryParams = CommentIdParams;

/// `comment.delete` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentDeleteParams {
    #[serde(alias = "comment_id")]
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "run_id")]
    pub actor_run_id: Option<i64>,
}

/// `comment.update` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentUpdateParams {
    #[serde(alias = "comment_id")]
    pub id: i64,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none", alias = "run_id")]
    pub actor_run_id: Option<i64>,
}

/// Current comment returned by management methods.
pub type CommentResult = CommentRecord;
pub type CommentGetResult = CommentRecord;
pub type CommentUpdateResult = CommentRecord;
pub type CommentDeleteResult = DeletedResult;

/// Audit snapshots returned by `comment.history`.
pub type CommentHistoryResult = Vec<CommentHistory>;

/// `run.done` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunDoneParams {
    pub card_id: i64,
    pub outcome: RunOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<i64>,
}

/// Internal `run.pane_exited` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunPaneExitedParams {
    pub card_id: i64,
    pub run_id: i64,
}

/// `run.cancel` / `run.retry` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCardParams {
    pub card_id: i64,
}

/// `run.focus` params. `origin_socket` identifies the invoking Herdr session.
///
/// `run_id` is **required**: the daemon never implicitly picks a run. Callers
/// that want "the newest run with a pane" resolve that themselves from the
/// card's run list and pass the exact id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFocusParams {
    pub card_id: i64,
    pub run_id: i64,
    pub origin_socket: String,
}

/// What `run.focus` actually did, so the caller can tell an ordinary jump from
/// a rescue instead of inferring it from the pane id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunFocusAction {
    /// The pane recorded on the run row was alive and got focus.
    FocusedRecordedPane,
    /// The recorded pane is gone, but an earlier rescue's pane for this run was
    /// still alive in the card tab, so that one got focus. No pane was created.
    FocusedRescuedPane,
    /// The recorded pane is gone; a **new** pane was created in the card tab and
    /// the harness conversation was resumed in it. The pane is ephemeral: it has
    /// no `runs` row, so the daemon does not own, watch, or time it out.
    Rescued,
}

/// `run.focus` result: the full identity of the run that was focused, so the
/// caller can say exactly *which* historical run it landed on, plus what the
/// daemon had to do to get there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFocusResult {
    /// Focus vs. rescue. Missing on older serialized payloads, which always
    /// meant an ordinary focus of the recorded pane.
    #[serde(default = "default_focus_action")]
    pub action: RunFocusAction,
    /// The pane the run row records, when it records one. On a rescue this is
    /// the **dead** pane, kept for diagnostics; `pane_id` is the live one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_pane_id: Option<String>,
    pub run_id: i64,
    pub card_id: i64,
    pub column_id: i64,
    pub harness: String,
    /// The **herdr session name** this run spawned into (`herdr --session <name>`),
    /// i.e. which Herdr instance/socket owns the pane. `None` = default session.
    /// This is NOT the harness conversation id — see `session_id`.
    pub session: Option<String>,
    /// The **harness conversation id** (Claude/Pi `--resume` id) recorded for
    /// this run. This is NOT a herdr session name — see `session`.
    pub session_id: Option<String>,
    /// The pane that now has focus — the recorded one, or the rescued one.
    pub pane_id: String,
}

fn default_focus_action() -> RunFocusAction {
    RunFocusAction::FocusedRecordedPane
}

/// `{run, card}` returned by run.done / run.cancel / run.retry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunActionResult {
    pub run: Run,
    pub card: Card,
}

// ---------------------------------------------------------------------------
// harness / space methods
// ---------------------------------------------------------------------------

/// `harness.capabilities` params. The result is a
/// [`HarnessCapabilities`](crate::capability::HarnessCapabilities); an unknown
/// harness yields error code 2 (not found).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessCapabilitiesParams {
    pub harness: String,
}

/// `harness.list` result: every harness the daemon knows about (built-ins
/// `pi`/`claude` plus every config-defined `[harness.NAME]`), sorted. Drives
/// the TUI harness/harness-override selects so they include config-defined
/// harnesses without a separate config read on the client.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessListResult {
    pub harnesses: Vec<String>,
}

/// A run space (herdr workspace) as surfaced by `space.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceInfo {
    pub id: String,
    pub label: String,
}

/// `space.list` params. `session` (`None` = default) scopes the listed
/// workspaces to that herdr session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// `space.list` result.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceListResult {
    pub spaces: Vec<SpaceInfo>,
}

/// A herdr session as surfaced by `session.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub name: String,
    pub default: bool,
    pub running: bool,
}

/// `session.list` result (no params).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionListResult {
    pub sessions: Vec<SessionInfo>,
}
