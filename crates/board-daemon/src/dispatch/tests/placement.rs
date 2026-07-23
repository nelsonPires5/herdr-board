use super::*;

#[test]
fn v11_placement_uses_run_session_while_legacy_uses_current_card_session() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let card = d
        .store
        .lock()
        .create_card(&CardCreateParams {
            title: "session snapshot".into(),
            session: Some("enqueue-session".into()),
            ..Default::default()
        })
        .unwrap();
    let mut run = enqueue_run(&d, card.id, card.column_id, false).unwrap();
    assert!(run.launch_spec.is_some());
    assert_eq!(run.session.as_deref(), Some("enqueue-session"));

    // Model a queued card edit in the dispatch snapshot: v11 ignores it.
    let mut edited_card = card;
    edited_card.session = Some("edited-session".into());
    assert_eq!(launch_session(&run, &edited_card), Some("enqueue-session"));

    // The same row shape without a v11 spec follows the documented legacy
    // adapter and therefore observes the current card session.
    run.launch_spec = None;
    assert_eq!(launch_session(&run, &edited_card), Some("edited-session"));
}

#[tokio::test]
async fn v7_and_pre_v7_launch_adapters_remain_explicit() {
    let spawner = Arc::new(CapturingSpawner::default());
    let mut config = Config::default();
    config.harness.insert(
        "custom".into(),
        board_core::config::HarnessDef {
            argv: vec!["custom".into()],
            ..Default::default()
        },
    );
    let (d, _, _) = test_daemon_with_config(spawner.clone(), config);
    let (v7_card, legacy_card, column_id) = {
        let db = d.store.lock();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "Adapters".into(),
                system_prompt: Some("current".into()),
                ..Default::default()
            })
            .unwrap();
        let v7 = db
            .create_card(&CardCreateParams {
                title: "v7".into(),
                column_id: Some(column.id),
                harness: Some("custom".into()),
                space_ref: Some("v7".into()),
                ..Default::default()
            })
            .unwrap();
        let legacy = db
            .create_card(&CardCreateParams {
                title: "legacy".into(),
                column_id: Some(column.id),
                harness: Some("custom".into()),
                space_ref: Some("legacy".into()),
                ..Default::default()
            })
            .unwrap();
        db.enqueue_run_uow(&EnqueueRun {
            card_id: v7.id,
            column_id: column.id,
            harness: "custom",
            argv_json: r#"["v7-command"]"#,
            prompt_snapshot: "v7-prompt",
            system_prompt_snapshot: Some("v7-system-exact"),
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
        db.enqueue_run_uow(&EnqueueRun {
            card_id: legacy.id,
            column_id: column.id,
            harness: "custom",
            argv_json: r#"["legacy-command"]"#,
            prompt_snapshot: "legacy-prompt",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
        (v7.id, legacy.id, column.id)
    };
    dispatch_pass(&d).await;
    let requests = spawner.requests.lock().unwrap();
    let v7 = requests.iter().find(|r| r.argv == ["v7-command"]).unwrap();
    assert!(v7
        .env
        .contains(&("BOARD_SYSTEM_PROMPT".into(), "v7-system-exact".into())));
    let legacy = requests
        .iter()
        .find(|r| r.argv == ["legacy-command"])
        .unwrap();
    assert!(legacy.env.contains(&(
        "BOARD_SYSTEM_PROMPT".into(),
        board_core::harness::protocol_system_prompt(Some("current"))
    )));
    assert_ne!(v7_card, legacy_card);
    assert!(column_id > 0);
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
fn existing_workspace_resolution_fails_when_snapshot_fails() {
    let (_dir, socket) = workspace_resolution_server(None);
    let mut client = HerdrClient::connect(&socket).unwrap();
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
        let (_dir, socket) = workspace_resolution_server(Some(missing_cwd_snapshot.clone()));
        let mut client = HerdrClient::connect(&socket).unwrap();
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
        let (_dir, socket) = new_workspace_resolution_server(snapshot);
        let mut client = HerdrClient::connect(&socket).unwrap();
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
    // RED: dispatch must gate the selected socket before resolve_space. A
    // mismatched socket must receive exactly ping; workspace.list/create,
    // session.snapshot, and spawner placement must not be reached.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("selected-herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let methods = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = Arc::clone(&methods);
    thread::spawn(move || {
        for stream in listener.incoming().take(3) {
            let Ok(stream) = stream else { break };
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 {
                continue;
            }
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            seen.lock()
                .unwrap()
                .push(request["method"].as_str().unwrap().into());
            let result = match request["method"].as_str().unwrap() {
                "ping" => serde_json::json!({
                    "type": "pong", "version": "0.7.4", "protocol": 17,
                    "capabilities": {}
                }),
                "workspace.list" => serde_json::json!({
                    "workspaces": [{
                        "workspace_id": "w1", "label": "feature", "number": 1,
                        "focused": false, "active_tab_id": "", "agent_status": "idle"
                    }]
                }),
                "session.snapshot" => serde_json::json!({}),
                other => panic!("unexpected mutating/placement method: {other}"),
            };
            writeln!(
                writer,
                "{}",
                serde_json::json!({
                    "id": request["id"], "result": result
                })
            )
            .unwrap();
            writer.flush().unwrap();
        }
    });

    let mut client = HerdrClient::connect(&socket).unwrap();
    let result = resolve_space(
        &mut client,
        SpaceKind::NewWorkspace,
        Some("feature"),
        Some("/tmp/feature"),
    );

    let actual_methods = methods.lock().unwrap().clone();
    assert_eq!(actual_methods, vec!["ping"]);
    let err = result.expect_err("protocol mismatch must stop workspace resolution");
    assert!(err
        .to_string()
        .contains("Herdr 0.7.5 with protocol 17 is required"));
}

// ---------------------------------------------------------------------------
// T13: manual enqueue vs auto-hop → identical persisted EnqueueRun fields
