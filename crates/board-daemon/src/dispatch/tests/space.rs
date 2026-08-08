//! Workspace / space resolution: reference lookup, `new_workspace` creation
//! and reuse, the live-snapshot cwd requirement, and the protocol preflight.

use super::*;

#[test]
fn resolve_ref_by_id_then_label() {
    let all = [ws("w1", "Alpha"), ws("w2", "Beta")];
    assert_eq!(resolve_workspace_ref(&all, "w2").unwrap(), "w2");
    // Case-insensitive label match.
    assert_eq!(resolve_workspace_ref(&all, "alpha").unwrap(), "w1");
}

#[test]
fn resolve_ref_unknown_lists_known() {
    let all = [ws("w1", "Alpha")];
    let err = resolve_workspace_ref(&all, "ghost").unwrap_err();
    assert!(err.contains("ghost"));
    assert!(err.contains("w1"));
}

#[test]
fn new_workspace_reuse_matches_label_case_insensitively() {
    let all = [ws("w1", "Alpha"), ws("w2", "MyFeature")];
    // Reuse: label already open → return its id (no create).
    assert_eq!(
        find_workspace_by_label(&all, "myfeature").as_deref(),
        Some("w2")
    );
}

#[test]
fn new_workspace_create_when_absent() {
    let all = [ws("w1", "Alpha")];
    // Absent → None → dispatch will call workspace.create.
    assert!(find_workspace_by_label(&all, "brand-new").is_none());
}

#[test]
fn new_workspace_create_carries_the_initial_tab_and_root_as_bootstrap() {
    // When `resolve_space` itself creates the workspace, the exact initial
    // tab/root pane of that brand-new workspace is returned as a one-shot
    // bootstrap hint for the first card-tab allocation. Reuse and existing
    // workspace resolution never carry one.
    let snapshot = serde_json::json!({
        "panes": [{
            "pane_id": "created-ws:p1",
            "workspace_id": "created-ws",
            "tab_id": "created-ws:t1",
            "cwd": "/repo",
            "focused": false,
            "revision": 1
        }]
    });
    let herdr = new_workspace_resolution_server_take(Some(snapshot), 5);
    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    let resolved = resolve_space(
        &mut client,
        SpaceKind::NewWorkspace,
        Some("Created"),
        Some("/requested-but-unverified"),
    )
    .expect("a created workspace with a live cwd resolves");
    assert_eq!(resolved.workspace_id, "created-ws");
    assert_eq!(resolved.cwd, "/repo");
    let bootstrap = resolved
        .bootstrap
        .expect("a workspace this resolution created must carry its initial tab");
    assert_eq!(bootstrap.tab_id, "created-ws:t1");
    assert_eq!(bootstrap.root_pane_id, "created-ws:p1");

    // The same label once open resolves by reuse and must NOT carry a hint.
    let herdr = workspace_resolution_server_take(
        Some(serde_json::json!({
            "panes": [{
                "pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1",
                "cwd": "/repo", "focused": false, "revision": 1
            }]
        })),
        4,
    );
    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    let reused = resolve_space(
        &mut client,
        SpaceKind::NewWorkspace,
        Some("feature"),
        Some("/repo"),
    )
    .expect("a label-matched open workspace resolves");
    assert_eq!(reused.workspace_id, "w1");
    assert_eq!(reused.cwd, "/repo");
    assert!(
        reused.bootstrap.is_none(),
        "a reused workspace must never supply a bootstrap hint"
    );
}

#[test]
fn existing_workspace_resolution_fails_when_snapshot_fails() {
    let herdr = workspace_resolution_server(None);
    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    let err = resolve_space(&mut client, SpaceKind::Workspace, Some("w1"), None)
        .expect_err("a snapshot failure must prevent launch without a cwd");
    assert!(err.to_string().contains("session snapshot unavailable"));
}

#[test]
fn workspace_resolution_fails_without_live_cwd_for_existing_and_reused_spaces() {
    let missing_cwd_snapshot = serde_json::json!({
        "panes": [{
            "pane_id": "w1:p1",
            "workspace_id": "w1",
            "focused": false,
            "revision": 1
        }]
    });

    for (kind, space_ref, space_cwd) in [
        (SpaceKind::Workspace, "w1", None),
        (SpaceKind::NewWorkspace, "Feature", Some("/fallback")),
    ] {
        let herdr = workspace_resolution_server(Some(missing_cwd_snapshot.clone()));
        let mut client = HerdrClient::connect(&herdr.socket).unwrap();
        let err = resolve_space(&mut client, kind, Some(space_ref), space_cwd)
            .expect_err("a missing live pane cwd must not fall back or be omitted");
        assert!(err.to_string().contains("cwd"), "{err}");
    }
}

#[test]
fn newly_created_workspace_requires_live_snapshot_cwd() {
    for snapshot in [
        None,
        Some(serde_json::json!({
            "panes": [{
                "pane_id": "created-ws:p1",
                "workspace_id": "created-ws",
                "focused": false,
                "revision": 1
            }]
        })),
    ] {
        let herdr = new_workspace_resolution_server(snapshot);
        let mut client = HerdrClient::connect(&herdr.socket).unwrap();
        let err = resolve_space(
            &mut client,
            SpaceKind::NewWorkspace,
            Some("Created"),
            Some("/requested-but-unverified"),
        )
        .expect_err("a created workspace must prove its cwd from a live pane snapshot");
        assert!(err.to_string().contains("cwd") || err.to_string().contains("snapshot"));
    }
}

#[test]
fn new_workspace_selected_socket_preflights_protocol_before_resolution() {
    // Dispatch must gate the selected socket before resolve_space. A
    // mismatched socket must receive exactly ping; workspace.list/create,
    // session.snapshot, and spawner placement must not be reached.
    let herdr = testkit::herdr_server()
        .version("0.8.1")
        .take(3)
        .on("workspace.list", |req| {
            testkit::reply(
                req,
                serde_json::json!({"workspaces": [{
                    "workspace_id": "w1", "label": "feature", "number": 1,
                    "focused": false, "active_tab_id": "", "agent_status": "idle"
                }]}),
            )
        })
        .on("session.snapshot", |req| {
            testkit::reply(req, serde_json::json!({}))
        })
        .serve();

    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    let result = resolve_space(
        &mut client,
        SpaceKind::NewWorkspace,
        Some("feature"),
        Some("/tmp/feature"),
    );

    assert_eq!(herdr.methods(), vec!["ping"]);
    let err = result.expect_err("protocol mismatch must stop workspace resolution");
    assert!(err.to_string().contains(&format!(
        "Herdr {} with protocol {} is required",
        board_herdr::SUPPORTED_HERDR_VERSION,
        board_herdr::SUPPORTED_HERDR_PROTOCOL
    )));
}

// ---------------------------------------------------------------------------
// T13: manual enqueue vs auto-hop → identical persisted EnqueueRun fields

#[test]
fn validate_space_resolvable_accepts_existing_workspace() {
    let herdr = workspace_resolution_server(None);
    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    // "w1" is in the fake workspace.list -> resolvable.
    validate_space_resolvable(&mut client, SpaceKind::Workspace, Some("w1"), None)
        .expect("an existing workspace resolves");
}

#[test]
fn validate_space_resolvable_rejects_missing_workspace() {
    let herdr = workspace_resolution_server(None);
    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    let err = validate_space_resolvable(&mut client, SpaceKind::Workspace, Some("ghost"), None)
        .expect_err("a missing workspace must not resolve");
    assert!(
        err.to_string().contains("not found"),
        "expected a not-found error, got: {err}"
    );
}

#[test]
fn validate_space_resolvable_new_workspace_only_needs_a_label() {
    let herdr = workspace_resolution_server(None);
    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    // new_workspace is created at run time, so the preflight only checks the
    // label is present (it still pings the session socket).
    validate_space_resolvable(
        &mut client,
        SpaceKind::NewWorkspace,
        Some("brand-new"),
        Some("/repo"),
    )
    .expect("a labelled new_workspace is structurally valid");
    let err = validate_space_resolvable(&mut client, SpaceKind::NewWorkspace, None, Some("/repo"))
        .expect_err("an empty new_workspace label must be rejected");
    assert!(err.to_string().contains("label"));
}
