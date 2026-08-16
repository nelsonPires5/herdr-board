//! Row structs mirroring `schema.sql`. These double as protocol result payloads.

use serde::{Deserialize, Serialize};

use crate::protocol::{
    AwaitingReason, CardLabels, CardStatus, Effort, RunOutcome, SpaceKind, Trigger,
};

/// A named collection of boards, identified by a canonical filesystem path.
/// `scope_path=None` is the special Global project; its title is `Global`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: i64,
    /// Folder-name title (`Global` for the special project).
    pub name: String,
    /// Canonical root path; `None` only for Global.
    pub scope_path: Option<String>,
}

impl Project {
    /// Folder-name title of a project scope; `Global` for the special project.
    /// A path without a file name (e.g. `/`) falls back to the path itself.
    pub fn display_name(scope_path: Option<&str>) -> String {
        match scope_path {
            None => "Global".into(),
            Some(path) => std::path::Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|n| !n.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| path.to_string()),
        }
    }
}

/// One board pipeline inside a [`Project`]. `scope_path` is the owning
/// project's canonical path (denormalized for wire compatibility); `None`
/// means the board belongs to the Global project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Board {
    pub id: i64,
    /// Missing on pre-v14 wire payloads: the only pre-v14 board rows were the
    /// Global board (id 1) and scoped boards, which migrated to project 1.
    #[serde(default = "default_project_id")]
    pub project_id: i64,
    pub name: String,
    pub scope_path: Option<String>,
}

fn default_project_id() -> i64 {
    1
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

/// Typed identity of a serialized execution space. The session is part of the
/// identity; `None` is the daemon's default session and remains distinct from
/// every explicitly named session. Null refs are preserved rather than encoded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpaceKey {
    pub session: Option<String>,
    pub kind: SpaceKind,
    pub reference: Option<String>,
}

impl SpaceKey {
    pub fn from_card(card: &Card) -> Self {
        Self {
            session: card.session.clone(),
            kind: card.space_kind,
            reference: card.space_ref.clone(),
        }
    }
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
    /// Read-only display labels stamped by the daemon (ready strings; the
    /// clients render them verbatim). Populated on read; never round-tripped.
    /// Missing on older serialized payloads, so default to empty.
    #[serde(default)]
    pub labels: CardLabels,
}

/// A timestamped note; author is `user`, `agent:<run_id>`, or `system`.
///
/// This is the compact, non-deleted projection used by prompts and ordinary
/// card lists. Deleted comments are intentionally not representable here so
/// existing prompt/client callers cannot accidentally render them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: i64,
    pub card_id: i64,
    pub author: String,
    pub body: String,
    pub created_at: String,
}

impl Comment {
    /// Whether the board itself wrote this comment (author `system`), as
    /// opposed to a human (`user`) or an agent run (`agent:<run_id>`).
    pub fn is_system(&self) -> bool {
        self.author == "system"
    }
}

/// The current comment row, including its soft-deletion marker. This separate
/// projection keeps the original `Comment` struct source-compatible for prompt
/// builders while exposing deletion state at management/audit boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentRecord {
    pub id: i64,
    pub card_id: i64,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub deleted_at: Option<String>,
}

/// Alias emphasizing that this is the current (as opposed to historical)
/// comment projection.
pub type CurrentComment = CommentRecord;
pub type CommentCurrent = CommentRecord;

/// One immutable snapshot in a comment's audit trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentHistory {
    pub id: i64,
    pub comment_id: i64,
    pub card_id: i64,
    pub author: String,
    pub body: String,
    pub created_at: String,
    pub deleted_at: Option<String>,
}

pub type CommentHistoryEntry = CommentHistory;
pub type CommentAudit = CommentHistory;

/// One agent execution of a card in a column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: i64,
    pub card_id: i64,
    pub column_id: i64,
    pub harness: String,
    pub argv_json: String,
    pub prompt_snapshot: String,
    /// Enqueue-time, protocol-trailer-inclusive system instructions. `None`
    /// identifies a legacy pre-v7 launch whose persisted argv is authoritative.
    #[serde(default, skip_serializing)]
    pub system_prompt_snapshot: Option<String>,
    /// Internal durable launch inputs; intentionally absent from board wire DTOs.
    #[serde(default, skip_serializing)]
    pub launch_spec: Option<crate::launch::RunLaunchSpec>,
    pub herdr_workspace_id: Option<String>,
    pub herdr_pane_id: Option<String>,
    /// Exact board-owned shell anchor pane for this card tab. This is
    /// internal placement identity, not a run target.
    #[serde(default, skip_serializing)]
    pub herdr_anchor_pane_id: Option<String>,
    /// harness conversation id (`--resume`); distinct from the herdr `session`.
    pub session_id: Option<String>,
    /// herdr session name this run spawned into; `None` = default session.
    pub session: Option<String>,
    pub started_at: Option<String>,
    /// Durable Unix-epoch millisecond deadline. `None` means unlimited.
    #[serde(default)]
    pub timeout_deadline_at_ms: Option<i64>,
    /// Unix-epoch milliseconds at which timeout accounting was paused.
    #[serde(default)]
    pub timeout_paused_at_ms: Option<i64>,
    pub ended_at: Option<String>,
    pub outcome: Option<RunOutcome>,
    pub result_summary: Option<String>,
    pub log_path: Option<String>,
}
