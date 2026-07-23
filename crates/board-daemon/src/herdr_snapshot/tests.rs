use board_herdr::{AgentStatus, PaneInfo, SessionSnapshot};

use super::snapshot_pane_statuses;

fn make_pane(pane_id: &str, agent_status: AgentStatus) -> PaneInfo {
    PaneInfo {
        pane_id: pane_id.to_string(),
        terminal_id: format!("t-{pane_id}"),
        workspace_id: "ws".into(),
        tab_id: "tab".into(),
        agent: None,
        agent_status,
        cwd: None,
        title: None,
        focused: false,
        revision: 0,
    }
}

fn empty_snapshot() -> SessionSnapshot {
    SessionSnapshot {
        version: "1".into(),
        protocol: 17,
        workspaces: vec![],
        tabs: vec![],
        panes: vec![],
        agents: vec![],
        focused_pane_id: None,
        focused_workspace_id: None,
    }
}

#[test]
fn empty_snapshot_yields_empty_map() {
    assert!(snapshot_pane_statuses(empty_snapshot()).is_empty());
}

#[test]
fn pane_fallback_when_no_agent() {
    let panes = vec![
        make_pane("p1", AgentStatus::Idle),
        make_pane("p2", AgentStatus::Working),
    ];
    let mut snap = empty_snapshot();
    snap.panes = panes;

    let statuses = snapshot_pane_statuses(snap);
    assert_eq!(statuses.len(), 2);
    assert_eq!(statuses["p1"], AgentStatus::Idle);
    assert_eq!(statuses["p2"], AgentStatus::Working);
}

#[test]
fn agent_status_overrides_pane_fallback() {
    let panes = vec![
        make_pane("p1", AgentStatus::Idle),
        make_pane("p2", AgentStatus::Working),
    ];
    let agents = vec![board_herdr::AgentInfo {
        pane_id: "p1".into(),
        terminal_id: "t-p1".into(),
        workspace_id: "ws".into(),
        tab_id: "tab".into(),
        agent: Some("pi".into()),
        agent_status: AgentStatus::Done,
        custom_status: None,
        focused: false,
        revision: 1,
        interactive_ready: true,
        launch_pending: false,
    }];
    let mut snap = empty_snapshot();
    snap.panes = panes;
    snap.agents = agents;

    let statuses = snapshot_pane_statuses(snap);
    assert_eq!(statuses.len(), 2);
    // Agent status overrides the pane fallback.
    assert_eq!(statuses["p1"], AgentStatus::Done);
    // No agent for p2 → pane fallback.
    assert_eq!(statuses["p2"], AgentStatus::Working);
}

#[test]
fn agent_on_nonexistent_pane_is_ignored() {
    let panes = vec![make_pane("p1", AgentStatus::Working)];
    let agents = vec![board_herdr::AgentInfo {
        pane_id: "orphan-agent".into(),
        terminal_id: "t-o".into(),
        workspace_id: "ws".into(),
        tab_id: "tab".into(),
        agent: Some("pi".into()),
        agent_status: AgentStatus::Done,
        custom_status: None,
        focused: false,
        revision: 1,
        interactive_ready: true,
        launch_pending: false,
    }];
    let mut snap = empty_snapshot();
    snap.panes = panes;
    snap.agents = agents;

    let statuses = snapshot_pane_statuses(snap);
    assert_eq!(statuses.len(), 1);
    // The orphan agent referencing a non-existent pane is simply not matched.
    assert_eq!(statuses["p1"], AgentStatus::Working);
}

#[test]
fn all_status_variants_roundtrip() {
    for expected in [
        AgentStatus::Idle,
        AgentStatus::Working,
        AgentStatus::Blocked,
        AgentStatus::Done,
        AgentStatus::Unknown,
    ] {
        let panes = vec![make_pane("p", expected)];
        let mut snap = empty_snapshot();
        snap.panes = panes;
        let statuses = snapshot_pane_statuses(snap);
        assert_eq!(statuses["p"], expected, "mismatch for {expected:?}");
    }
}
