//! Row structs mirroring `schema.sql`. These double as protocol result payloads.

use serde::{Deserialize, Serialize};

use crate::protocol::{AwaitingReason, CardStatus, Effort, RunOutcome, SpaceKind, Trigger};

/// One independent board pipeline. `scope_path=None` is the preserved Global board.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    pub id: i64,
    pub name: String,
    pub scope_path: Option<String>,
}

/// A pipeline stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub id: i64,
    pub board_id: i64,
    pub name: String,
    pub position: i64,
    pub system_prompt: Option<String>,
    pub trigger: Trigger,
    pub on_success_column_id: Option<i64>,
    pub on_fail_column_id: Option<i64>,
    pub fresh_session: bool,
    pub harness_override: Option<String>,
    pub model_override: Option<String>,
    pub effort_override: Option<String>,
    pub permission_override: Option<String>,
    pub timeout_minutes: Option<i64>,
}

/// A unit of work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub id: i64,
    pub board_id: i64,
    pub column_id: i64,
    pub position: i64,
    pub title: String,
    pub description: String,
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    pub permission_mode: Option<String>,
    /// herdr session name; `None` = the daemon's default session.
    pub session: Option<String>,
    pub space_kind: SpaceKind,
    /// Workspace id (kind `workspace`) or new-workspace label (kind `new_workspace`).
    pub space_ref: Option<String>,
    /// Working directory for a `new_workspace` space; `None` otherwise.
    pub space_cwd: Option<String>,
    pub status: CardStatus,
    /// Why the card is `awaiting`; `None` unless `status == awaiting`.
    pub awaiting_reason: Option<AwaitingReason>,
    /// Harness conversation id for `--resume` (distinct from `session`).
    pub session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    /// When the card was archived; `None` means it is active on the board.
    pub archived_at: Option<String>,
}

/// A timestamped note; author is `user`, `agent:<run_id>`, or `system`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: i64,
    pub card_id: i64,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

/// One agent execution of a card in a column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: i64,
    pub card_id: i64,
    pub column_id: i64,
    pub harness: String,
    pub argv_json: String,
    pub prompt_snapshot: String,
    pub herdr_workspace_id: Option<String>,
    pub herdr_pane_id: Option<String>,
    /// harness conversation id (`--resume`); distinct from the herdr `session`.
    pub session_id: Option<String>,
    /// herdr session name this run spawned into; `None` = default session.
    pub session: Option<String>,
    pub started_at: Option<String>,
    pub ended_at: Option<String>,
    pub outcome: Option<RunOutcome>,
    pub result_summary: Option<String>,
    pub log_path: Option<String>,
}
