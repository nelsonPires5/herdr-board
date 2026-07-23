//! Pure conversion helpers over board-herdr types, shared by watchers and
//! supervisor. No I/O or side effects.

use std::collections::HashMap;

use board_herdr::{AgentStatus, SessionSnapshot};

/// Convert a [`SessionSnapshot`] to a `HashMap<pane_id, AgentStatus>`.
///
/// Agent status (when present on the matching [`board_herdr::AgentInfo`])
/// takes precedence; otherwise the pane's own `agent_status` is the fallback.
/// Duplicate pane ids are impossible (the snapshot is authoritative), so the
/// last-write-wins semantics of the collecting iterator is benign.
pub fn snapshot_pane_statuses(snapshot: SessionSnapshot) -> HashMap<String, AgentStatus> {
    snapshot
        .panes
        .into_iter()
        .map(|pane| {
            let status = snapshot
                .agents
                .iter()
                .find(|agent| agent.pane_id == pane.pane_id)
                .map(|agent| agent.agent_status)
                .unwrap_or(pane.agent_status);
            (pane.pane_id, status)
        })
        .collect()
}

#[cfg(test)]
mod tests;
