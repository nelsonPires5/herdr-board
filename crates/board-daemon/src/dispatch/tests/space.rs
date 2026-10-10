//! Workspace / space resolution: reference lookup, `new_workspace` creation
//! and reuse, the live-snapshot cwd requirement, and the protocol preflight.

use super::*;

#[test]
fn concurrent_same_label_resolution_creates_one_workspace_and_one_bootstrap() {
    let created = Arc::new(AtomicUsize::new(0));
    let state = created.clone();
    let herdr = testkit::herdr_server()
        .handler(move |req, _| {
            let workspace = serde_json::json!({
                "workspace_id": "w1", "label": "Shared", "number": 1,
                "focused": false, "active_tab_id": "w1:t1", "agent_status": "unknown"
            });
            match req["method"].as_str().unwrap() {
                "workspace.list" => testkit::reply(
                    req,
                    serde_json::json!({
                        "workspaces": if state.load(Ordering::SeqCst) == 0 {
                            vec![]
                        } else {
                            vec![workspace]
                        }
                    }),
                ),
                "workspace.create" => {
                    state.fetch_add(1, Ordering::SeqCst);
                    testkit::reply(
                        req,
                        serde_json::json!({
                            "workspace": workspace,
                            "tab": {"tab_id": "w1:t1", "workspace_id": "w1", "number": 1,
                                "label": "tab", "focused": false, "pane_count": 1},
                            "root_pane": testkit::pane_info("w1:p1")
                        }),
                    )
                }
                "session.snapshot" => {
                    let mut pane = testkit::pane_info("w1:p1");
                    pane["cwd"] = Value::String("/repo".into());
                    testkit::reply(req, serde_json::json!({"snapshot": {"panes": [pane]}}))
                }
                method => panic!("unexpected workspace resolution method {method}"),
            }
        })
        .serve();
    let aliases = tempfile::tempdir().unwrap();
    let alias = aliases.path().join("alias.sock");
    std::os::unix::fs::symlink(&herdr.socket, &alias).unwrap();
    let start = Arc::new(std::sync::Barrier::new(12));
    let resolutions = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..12)
            .map(|index| {
                let socket = if index % 2 == 0 {
                    herdr.socket.clone()
                } else {
                    alias.clone()
                };
                let start = start.clone();
                scope.spawn(move || {
                    let mut client = HerdrClient::connect(&socket).unwrap();
                    start.wait();
                    resolve_space(
                        &mut client,
                        SpaceKind::NewWorkspace,
                        Some(if index % 2 == 0 { "Shared" } else { "SHARED" }),
                        Some("/repo"),
                    )
                    .unwrap()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(created.load(Ordering::SeqCst), 1);
    assert_eq!(
        resolutions.iter().filter(|s| s.bootstrap.is_some()).count(),
        1
    );
    assert!(resolutions
        .iter()
        .all(|s| s.workspace_id == "w1" && s.cwd == "/repo"));
}

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
fn workspace_resolution_honors_explicit_cwd_without_reading_the_snapshot() {
    // An explicit card cwd is the operator's deterministic choice for a
    // workspace whose live panes may intentionally use different directories.
    // The three-connection budget proves resolution stops after the connect
    // probe, protocol gate, and workspace.list.
    let herdr = workspace_resolution_server_take(None, 3);
    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    let resolved = resolve_space(&mut client, SpaceKind::Workspace, Some("w1"), Some("/repo"))
        .expect("an explicit cwd must not depend on pane ordering or snapshot availability");

    assert_eq!(resolved.workspace_id, "w1");
    assert_eq!(resolved.cwd, "/repo");
    assert_eq!(herdr.methods(), vec!["ping", "workspace.list"]);
}

#[test]
fn workspace_resolution_rejects_heterogeneous_live_cwds_without_override() {
    let snapshot = serde_json::json!({
        "panes": [
            {
                "pane_id": "w1:p2", "workspace_id": "w1", "tab_id": "w1:t2",
                "cwd": "/repo/claude", "focused": false, "revision": 1
            },
            {
                "pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1",
                "cwd": "/repo", "focused": true, "revision": 1
            }
        ]
    });
    let herdr = workspace_resolution_server(Some(snapshot));
    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    let err = resolve_space(&mut client, SpaceKind::Workspace, Some("w1"), None)
        .expect_err("pane ordering must not silently choose a cwd");
    let message = err.to_string();

    assert!(message.contains("multiple live pane cwd"), "{message}");
    assert!(message.contains("w1:p1=/repo"), "{message}");
    assert!(message.contains("w1:p2=/repo/claude"), "{message}");
    assert!(message.contains("space_cwd"), "{message}");
}

#[test]
fn new_workspace_reuse_rejects_heterogeneous_live_cwds_with_reuse_specific_advice() {
    // A reused `new_workspace` card deliberately ignores its `space_cwd`, so
    // the generic "set an explicit space_cwd" advice would be unusable here;
    // the error must point at the real remedy instead.
    let snapshot = serde_json::json!({
        "panes": [
            {
                "pane_id": "w1:p2", "workspace_id": "w1", "tab_id": "w1:t2",
                "cwd": "/repo/claude", "focused": false, "revision": 1
            },
            {
                "pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1",
                "cwd": "/repo", "focused": true, "revision": 1
            }
        ]
    });
    let herdr = workspace_resolution_server(Some(snapshot));
    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    let err = resolve_space(
        &mut client,
        SpaceKind::NewWorkspace,
        Some("Feature"),
        Some("/fallback"),
    )
    .expect_err("heterogeneous live cwds must fail on a reused new_workspace too");
    let message = err.to_string();

    assert!(message.contains("new_workspace"), "{message}");
    assert!(
        message.contains("make the live pane cwds consistent"),
        "{message}"
    );
    assert!(
        message.contains("space_cwd is deliberately not applied"),
        "{message}"
    );
}

#[test]
fn workspace_resolution_accepts_multiple_panes_with_the_same_cwd() {
    let snapshot = serde_json::json!({
        "panes": [
            {
                "pane_id": "w1:p1", "workspace_id": "w1", "tab_id": "w1:t1",
                "cwd": "/repo", "focused": true, "revision": 1
            },
            {
                "pane_id": "w1:p2", "workspace_id": "w1", "tab_id": "w1:t2",
                "cwd": "/repo", "focused": false, "revision": 1
            }
        ]
    });
    let herdr = workspace_resolution_server(Some(snapshot));
    let mut client = HerdrClient::connect(&herdr.socket).unwrap();
    let resolved = resolve_space(&mut client, SpaceKind::Workspace, Some("w1"), None)
        .expect("equivalent pane cwds are unambiguous");

    assert_eq!(resolved.cwd, "/repo");
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
    // Protocol is the hard gate: wrong protocol (21) is incompatible,
    // any version with protocol 22 is compatible.
    let herdr = testkit::herdr_server()
        .protocol(board_herdr::SUPPORTED_HERDR_PROTOCOL - 1)
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
        "Herdr socket protocol {} is required",
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
