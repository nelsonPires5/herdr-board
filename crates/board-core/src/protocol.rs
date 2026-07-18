//! boardd socket protocol (v1) — serde types, the single source of truth.
//!
//! See `docs/protocol.md` for semantics. Transport is newline-delimited JSON over a
//! Unix socket; every request/response/event and every method's params/result is
//! represented here.

use serde::{Deserialize, Serialize};

use crate::model::{Board, Card, Column, Comment, Run};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceKind {
    Workspace,
    NewWorkspace,
}

/// Live card status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CardStatus {
    Idle,
    Queued,
    Running,
    Blocked,
    Failed,
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
});
str_enum!(RunOutcome {
    Ok => "ok", Fail => "fail", Cancelled => "cancelled", Lost => "lost",
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
        Response {
            id: id.into(),
            result: None,
            error: Some(RpcError {
                code,
                message: message.into(),
            }),
        }
    }
}

/// Structured error payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
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

/// `board.get` / `board.open` result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub board: Board,
    pub columns: Vec<Column>,
    pub cards: Vec<Card>,
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
    /// Working directory for a `new_workspace` space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_cwd: Option<String>,
}

/// `card.archive` params — archive (`true`) or restore (`false`) a card.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardArchiveParams {
    pub id: i64,
    pub archived: bool,
}

/// `card.move` params — the dispatch trigger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CardMoveParams {
    pub id: i64,
    pub column_id: i64,
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
}

/// `run.done` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunDoneParams {
    pub card_id: i64,
    pub outcome: RunOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// `run.cancel` / `run.retry` params.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCardParams {
    pub card_id: i64,
}

/// `run.focus` params. `origin_socket` identifies the invoking Herdr session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFocusParams {
    pub card_id: i64,
    pub origin_socket: String,
}

/// `run.focus` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFocusResult {
    pub run_id: i64,
    pub pane_id: String,
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
