//! Card-tab ownership: what a card tab's Herdr identity may be reconstructed
//! from after a restart, and which durable panes count as board-owned.

use super::*;

#[test]
fn card_tab_reconstruction_requires_a_durable_owned_pane_not_a_label() {
    let pane = |pane_id: &str, workspace_id: &str, tab_id: &str| PaneInfo {
        pane_id: pane_id.into(),
        terminal_id: format!("terminal-{pane_id}"),
        workspace_id: workspace_id.into(),
        tab_id: tab_id.into(),
        label: None,
        agent: None,
        agent_status: AgentStatus::Unknown,
        title: None,
        cwd: None,
        focused: false,
        revision: 1,
    };
    let snapshot = SessionSnapshot {
        panes: vec![
            pane("user-pane", "w1", "w1:user-tab"),
            pane("owned-pane", "w1", "w1:owned-card-tab"),
        ],
        ..Default::default()
    };
    assert_eq!(
        reconstruct_owned_tab_id(&snapshot, "w1", &["owned-pane".into()]).as_deref(),
        Some("w1:owned-card-tab")
    );
    assert_eq!(
        reconstruct_owned_tab_id(&snapshot, "w1", &["missing-pane".into()]),
        None
    );
    assert_eq!(
        reconstruct_owned_tab_id(&snapshot, "w2", &["owned-pane".into()]),
        None
    );
}

#[test]
fn card_tab_reconstruction_prefers_newest_durable_pane_order_not_snapshot_order() {
    let pane = |pane_id: &str, tab_id: &str| PaneInfo {
        pane_id: pane_id.into(),
        terminal_id: format!("terminal-{pane_id}"),
        workspace_id: "w1".into(),
        tab_id: tab_id.into(),
        label: None,
        agent: None,
        agent_status: AgentStatus::Unknown,
        title: None,
        cwd: None,
        focused: false,
        revision: 1,
    };
    // The run list is newest-first, while Herdr's snapshot ordering is not an
    // ownership signal. Reconstruction must therefore use the durable order.
    let snapshot = SessionSnapshot {
        panes: vec![
            pane("older-pane", "w1:older-tab"),
            pane("newer-pane", "w1:newer-tab"),
        ],
        ..Default::default()
    };
    assert_eq!(
        reconstruct_owned_tab_id(&snapshot, "w1", &["newer-pane".into(), "older-pane".into()])
            .as_deref(),
        Some("w1:newer-tab")
    );
}

#[test]
fn durable_card_tab_panes_are_scoped_to_v12_session_and_workspace_ownership() {
    use board_core::launch::{ExecutionSpec, RunLaunchSpec};

    let launch_spec = || {
        Some(RunLaunchSpec::v1(ExecutionSpec {
            argv: Vec::new(),
            env: Vec::new(),
            agent_kind: None,
            initial_prompt: None,
            system_prompt: None,
        }))
    };
    let run =
        |id: i64, pane: &str, session: Option<&str>, workspace: Option<&str>, durable: bool| Run {
            id,
            card_id: 1,
            column_id: 1,
            harness: "fake".into(),
            argv_json: "[]".into(),
            prompt_snapshot: String::new(),
            system_prompt_snapshot: None,
            launch_spec: durable.then(|| launch_spec().expect("test launch spec")),
            herdr_workspace_id: workspace.map(str::to_string),
            herdr_pane_id: Some(pane.into()),
            herdr_anchor_pane_id: None,
            session_id: None,
            session: session.map(str::to_string),
            started_at: Some("now".into()),
            timeout_deadline_at_ms: None,
            timeout_paused_at_ms: None,
            ended_at: None,
            outcome: None,
            result_summary: None,
            log_path: None,
        };
    let mut runs = vec![
        run(1, "legacy", Some("s1"), Some("w1"), false),
        run(2, "wrong-session", Some("s2"), Some("w1"), true),
        run(3, "older", Some("s1"), Some("w1"), true),
        run(4, "wrong-workspace", Some("s1"), Some("w2"), true),
        run(5, "newer", Some("s1"), Some("w1"), true),
    ];
    assert_eq!(
        owned_pane_ids(&runs, Some("s1"), "w1", OwnedPanes::DurableChildren),
        ["newer".to_string(), "older".to_string()]
    );
    runs[2].herdr_anchor_pane_id = Some("anchor-older".into());
    runs[4].herdr_anchor_pane_id = Some("anchor-newer".into());
    assert_eq!(
        owned_pane_ids(&runs, Some("s1"), "w1", OwnedPanes::Anchors),
        ["anchor-newer".to_string(), "anchor-older".to_string()]
    );
}

// T6: after reuse, consecutive runs share one `herdr_pane_id`. Tab ownership
// is still reconstructed from that exact durable pane identity — no pane is
// orphaned, and reclaimable/durable partitioning keeps working with the dup.
#[test]
fn shared_pane_after_reuse_still_resolves_one_owned_tab() {
    use board_core::launch::{ExecutionSpec, RunLaunchSpec};

    let launch_spec = || {
        Some(RunLaunchSpec::v1(ExecutionSpec {
            argv: Vec::new(),
            env: Vec::new(),
            agent_kind: None,
            initial_prompt: None,
            system_prompt: None,
        }))
    };
    let run =
        |id: i64, pane: &str, session: Option<&str>, workspace: Option<&str>, ended: bool| Run {
            id,
            card_id: 1,
            column_id: 1,
            harness: "pi".into(),
            argv_json: "[]".into(),
            prompt_snapshot: String::new(),
            system_prompt_snapshot: None,
            launch_spec: launch_spec(),
            herdr_workspace_id: workspace.map(str::to_string),
            herdr_pane_id: Some(pane.into()),
            herdr_anchor_pane_id: None,
            session_id: Some("conv-1".into()),
            session: session.map(str::to_string),
            started_at: Some("now".into()),
            timeout_deadline_at_ms: None,
            timeout_paused_at_ms: None,
            ended_at: ended.then(|| "done".into()),
            outcome: None,
            result_summary: None,
            log_path: None,
        };
    // run1 was the fresh hop (pane p0, ended); run2 and run3 are resume hops
    // that reused the SAME pane p1 (run2 ended, run3 still open).
    let runs = vec![
        run(1, "w1:p0", Some("s1"), Some("w1"), true),
        run(2, "w1:p1", Some("s1"), Some("w1"), true),
        run(3, "w1:p1", Some("s1"), Some("w1"), false),
    ];
    // Newest durable run first; the shared pane appears once per run that
    // recorded it.
    assert_eq!(
        owned_pane_ids(&runs, Some("s1"), "w1", OwnedPanes::DurableChildren),
        [
            "w1:p1".to_string(),
            "w1:p1".to_string(),
            "w1:p0".to_string()
        ]
    );
    // Only ended runs are reclaimable; the open run3 is excluded.
    assert_eq!(
        owned_pane_ids(&runs, Some("s1"), "w1", OwnedPanes::ReclaimableChildren),
        ["w1:p1".to_string(), "w1:p0".to_string()]
    );
    // The single board-owned tab resolves from the shared durable pane.
    let pane = |pane_id: &str, tab_id: &str| PaneInfo {
        pane_id: pane_id.into(),
        terminal_id: format!("term-{pane_id}"),
        workspace_id: "w1".into(),
        tab_id: tab_id.into(),
        label: None,
        agent: None,
        agent_status: AgentStatus::Unknown,
        title: None,
        cwd: None,
        focused: false,
        revision: 1,
    };
    let snapshot = SessionSnapshot {
        panes: vec![pane("w1:p1", "w1:t-card"), pane("w1:p0", "w1:t-card")],
        ..Default::default()
    };
    assert_eq!(
        reconstruct_owned_tab_id(&snapshot, "w1", &["w1:p1".into()]).as_deref(),
        Some("w1:t-card")
    );
}
