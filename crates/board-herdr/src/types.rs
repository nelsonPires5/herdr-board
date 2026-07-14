//! Typed views over herdr result payloads.
//!
//! Field names verified against `herdr api schema --json` (protocol 16,
//! captured in `tests/fixtures/schema.json`). All structs use
//! `#[serde(default)]` on optional fields and ignore unknown fields so the
//! client keeps working across minor herdr additions.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Agent status as reported by herdr. `idle` != finished — "done" is only
/// produced by integrations that call `pane report-agent --state done`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

/// Direction for a split when starting an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    Right,
    Down,
}

/// Where to read pane text from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadSource {
    Visible,
    Recent,
    RecentUnwrapped,
    Detection,
}

/// Notification sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSound {
    None,
    Done,
    Request,
}

/// A workspace, as it appears in snapshots and `workspace.list`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub active_tab_id: String,
    #[serde(default = "unknown_status")]
    pub agent_status: AgentStatus,
}

/// A tab, as it appears in snapshots and `tab.list`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TabInfo {
    pub tab_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub number: u64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub pane_count: u64,
    #[serde(default = "unknown_status")]
    pub agent_status: AgentStatus,
}

/// A pane. `terminal_id` is the stable id of the underlying terminal; `pane_id`
/// is the layout slot. Both are needed by the daemon's spawn handle.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PaneInfo {
    pub pane_id: String,
    #[serde(default)]
    pub terminal_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default = "unknown_status")]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
}

/// An agent-bearing pane, as listed in `session.snapshot.agents`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AgentInfo {
    pub pane_id: String,
    #[serde(default)]
    pub terminal_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default = "unknown_status")]
    pub agent_status: AgentStatus,
    #[serde(default)]
    pub custom_status: Option<String>,
}

/// A git worktree entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorktreeInfo {
    pub path: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub open_workspace_id: Option<String>,
}

/// Live session state (subset the daemon consumes).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionSnapshot {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol: u32,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceInfo>,
    #[serde(default)]
    pub tabs: Vec<TabInfo>,
    #[serde(default)]
    pub panes: Vec<PaneInfo>,
    #[serde(default)]
    pub agents: Vec<AgentInfo>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
}

impl SessionSnapshot {
    /// Whether a pane still exists in this snapshot (i.e. has not exited/closed).
    pub fn pane_exists(&self, pane_id: &str) -> bool {
        self.panes.iter().any(|p| p.pane_id == pane_id)
    }

    /// Whether a pane exists and is not in a terminal (`done`) agent state.
    /// This is the daemon's liveness signal for a spawned agent pane.
    pub fn pane_alive(&self, pane_id: &str) -> bool {
        self.panes
            .iter()
            .any(|p| p.pane_id == pane_id && p.agent_status != AgentStatus::Done)
    }

    /// Look up an agent-bearing pane by id.
    pub fn agent(&self, pane_id: &str) -> Option<&AgentInfo> {
        self.agents.iter().find(|a| a.pane_id == pane_id)
    }
}

/// Result of `workspace.create`.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceCreated {
    pub workspace: WorkspaceInfo,
    pub tab: TabInfo,
    pub root_pane: PaneInfo,
}

impl WorkspaceCreated {
    pub fn workspace_id(&self) -> &str {
        &self.workspace.workspace_id
    }
    pub fn root_pane_id(&self) -> &str {
        &self.root_pane.pane_id
    }
}

/// Result of `tab.create`.
#[derive(Debug, Clone, Deserialize)]
pub struct TabCreated {
    pub tab: TabInfo,
    pub root_pane: PaneInfo,
}

/// Result of `agent.start`. `argv` echoes the launched command line.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentStarted {
    pub agent: AgentInfo,
    #[serde(default)]
    pub argv: Vec<String>,
}

impl AgentStarted {
    pub fn pane_id(&self) -> &str {
        &self.agent.pane_id
    }
    pub fn terminal_id(&self) -> &str {
        &self.agent.terminal_id
    }
    pub fn workspace_id(&self) -> &str {
        &self.agent.workspace_id
    }
}

/// Result of `worktree.create`.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeCreated {
    pub workspace: WorkspaceInfo,
    pub tab: TabInfo,
    pub root_pane: PaneInfo,
    pub worktree: WorktreeInfo,
}

impl WorktreeCreated {
    pub fn path(&self) -> &str {
        &self.worktree.path
    }
    pub fn workspace_id(&self) -> &str {
        &self.workspace.workspace_id
    }
    pub fn root_pane_id(&self) -> &str {
        &self.root_pane.pane_id
    }
}

/// Result of `worktree.remove`.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeRemoved {
    pub workspace_id: String,
    pub path: String,
    #[serde(default)]
    pub forced: bool,
}

/// Result of `pane.read`.
#[derive(Debug, Clone, Deserialize)]
pub struct PaneReadResult {
    pub pane_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub truncated: bool,
}

/// Result of `notification.show`.
#[derive(Debug, Clone, Deserialize)]
pub struct NotificationShown {
    pub shown: bool,
    #[serde(default)]
    pub reason: String,
}

/// Result of `ping` (used for liveness).
#[derive(Debug, Clone, Deserialize)]
pub struct Pong {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol: u32,
    #[serde(default)]
    pub capabilities: BTreeMap<String, serde_json::Value>,
}

/// A rectangle in terminal cells, as reported inside a [`Layout`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Rect {
    #[serde(default)]
    pub x: u64,
    #[serde(default)]
    pub y: u64,
    #[serde(default)]
    pub width: u64,
    #[serde(default)]
    pub height: u64,
}

/// A pane slot within a [`Layout`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LayoutPane {
    pub pane_id: String,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub rect: Rect,
}

/// A split node within a [`Layout`]. `direction` is `right`/`down` (kept as a
/// string for forward-compatibility); `ratio` is the first child's fraction.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct LayoutSplit {
    pub id: String,
    #[serde(default)]
    pub direction: String,
    #[serde(default)]
    pub ratio: f64,
    #[serde(default)]
    pub rect: Rect,
}

/// The pane layout of a tab (result of `pane.layout`): each pane's rectangle
/// plus the split tree.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Layout {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub zoomed: bool,
    #[serde(default)]
    pub area: Rect,
    #[serde(default)]
    pub focused_pane_id: String,
    #[serde(default)]
    pub panes: Vec<LayoutPane>,
    #[serde(default)]
    pub splits: Vec<LayoutSplit>,
}

fn unknown_status() -> AgentStatus {
    AgentStatus::Unknown
}
