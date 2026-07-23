//! Public parameter structs for herdr RPC calls.
//!
//! Every struct serializes to the exact field names and shape expected by the
//! herdr socket protocol (protocol 17). Optional fields use
//! `#[serde(skip_serializing_if)]` so they are omitted when absent, matching
//! the wire behaviour of the `herdr` CLI.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::types::{AgentStatus, SplitDirection};

/// Params for `workspace.create`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WorkspaceCreateParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub focus: bool,
}

/// Params for `tab.create`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TabCreateParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub focus: bool,
}

/// Protocol-17 params for `agent.start`. Placement, cwd, and environment are
/// established on the target pane before starting the managed agent.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentStartParams {
    pub name: String,
    pub kind: String,
    pub pane_id: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Optional wait behavior for `agent.prompt`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentPromptWaitOptions {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Params for `agent.prompt`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentPromptParams {
    pub target: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<AgentPromptWaitOptions>,
}

/// Params for `agent.wait`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AgentWaitParams {
    pub target: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

/// Params for protocol-17 `pane.split`.
#[derive(Debug, Clone, Serialize)]
pub struct PaneSplitParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub target_pane_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    pub direction: SplitDirection,
    pub focus: bool,
}

/// Params for `pane.rename`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PaneRenameParams {
    pub pane_id: String,
    pub label: String,
}
