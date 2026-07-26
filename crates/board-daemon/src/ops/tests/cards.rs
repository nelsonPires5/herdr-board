use super::*;

#[test]
fn card_delete_rejects_and_preserves_queued_blocked_and_awaiting_open_runs() {
    for (status, started) in [
        (CardStatus::Queued, false),
        (CardStatus::Blocked, true),
        (CardStatus::Awaiting, true),
    ] {
        let d = test_daemon(Config::default());
        let (card_id, run_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: format!("{status:?}"),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .enqueue_run_uow(&EnqueueRun {
                    card_id: card.id,
                    column_id: card.column_id,
                    harness: "pi",
                    argv_json: "[]",
                    prompt_snapshot: "p",
                    system_prompt_snapshot: None,
                    launch_spec_json: None,
                    session_id: None,
                    session: None,
                })
                .unwrap();
            if started {
                db.promote_run_uow(run.id, Some("w1"), Some("p1"), None)
                    .unwrap();
            }
            if status == CardStatus::Awaiting {
                db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
                    .unwrap();
            } else {
                db.set_card_status(card.id, status).unwrap();
            }
            (card.id, run.id)
        };

        let err = handle_request(&d, "card.delete", json!({"id": card_id})).unwrap_err();
        assert_eq!(err.code(), 3);
        assert!(err.to_string().contains("open run"));
        let db = d.store.lock();
        assert!(db.get_card(card_id).unwrap().is_some());
        assert!(db.get_run(run_id).unwrap().ended_at.is_none());
    }
}

#[test]
fn card_locked_field_update_rejects_queued_blocked_and_awaiting_open_runs() {
    for (status, started) in [
        (CardStatus::Queued, false),
        (CardStatus::Blocked, true),
        (CardStatus::Awaiting, true),
    ] {
        let d = test_daemon(Config::default());
        let card_id = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: format!("{status:?}"),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .enqueue_run_uow(&EnqueueRun {
                    card_id: card.id,
                    column_id: card.column_id,
                    harness: "pi",
                    argv_json: "[]",
                    prompt_snapshot: "p",
                    system_prompt_snapshot: None,
                    launch_spec_json: None,
                    session_id: None,
                    session: None,
                })
                .unwrap();
            if started {
                db.promote_run_uow(run.id, Some("w1"), Some("p1"), None)
                    .unwrap();
            }
            if status == CardStatus::Awaiting {
                db.set_card_awaiting(card.id, AwaitingReason::IdleExpired)
                    .unwrap();
            } else {
                db.set_card_status(card.id, status).unwrap();
            }
            card.id
        };

        let err = handle_request(
            &d,
            "card.update",
            json!({"id": card_id, "model": "locked-model"}),
        )
        .unwrap_err();
        assert_eq!(err.code(), 3);
        assert!(err.to_string().contains("open run"));

        // Unlocked metadata remains editable while a run is open.
        let updated = handle_request(
            &d,
            "card.update",
            json!({"id": card_id, "title": "new title"}),
        )
        .unwrap();
        assert_eq!(updated["title"], "new title");
    }
}

#[test]
fn card_open_run_db_guard_wins_over_stale_nonbusy_status() {
    let d = test_daemon(Config::default());
    let card_id = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "stale status".into(),
                ..Default::default()
            })
            .unwrap();
        db.enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "p",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
        db.set_card_status(card.id, CardStatus::Done).unwrap();
        card.id
    };

    let edit_err = handle_request(
        &d,
        "card.update",
        json!({"id": card_id, "model": "locked-model"}),
    )
    .unwrap_err();
    assert_eq!(edit_err.code(), 3);
    assert!(edit_err.to_string().contains("open run"));

    let archive_err =
        handle_request(&d, "card.archive", json!({"id": card_id, "archived": true})).unwrap_err();
    assert_eq!(archive_err.code(), 3);
    assert!(archive_err.to_string().contains("open run"));
}

#[test]
fn card_archive_roundtrip_and_busy_rejection() {
    let d = test_daemon(Config::default());
    let created = handle_request(&d, "card.create", json!({ "title": "archive me" })).unwrap();
    let id = created["id"].as_i64().unwrap();

    let archived =
        handle_request(&d, "card.archive", json!({ "id": id, "archived": true })).unwrap();
    assert!(archived["archived_at"].is_string());

    let restored =
        handle_request(&d, "card.archive", json!({ "id": id, "archived": false })).unwrap();
    assert!(restored["archived_at"].is_null());

    d.store
        .lock()
        .set_card_status(id, CardStatus::Running)
        .unwrap();
    let err =
        handle_request(&d, "card.archive", json!({ "id": id, "archived": true })).unwrap_err();
    assert_eq!(err.code(), 3);
    assert!(err.to_string().contains("cancel it before archiving"));
}

#[test]
fn archived_card_cannot_move_until_restored() {
    let d = test_daemon(Config::default());
    let created = handle_request(&d, "card.create", json!({ "title": "inert" })).unwrap();
    let id = created["id"].as_i64().unwrap();
    handle_request(&d, "card.archive", json!({ "id": id, "archived": true })).unwrap();
    let err = handle_request(&d, "card.move", json!({ "id": id, "column_id": 1 })).unwrap_err();
    assert_eq!(err.code(), 3);
    assert!(err.to_string().contains("restored before moving"));
}

// --- cross-board transfer + blocking sanity check (prototype) ---

fn scoped_board(d: &Arc<Daemon>, path: &str) -> i64 {
    let snap = handle_request(d, "board.open", json!({ "scope_path": path })).unwrap();
    snap["board"]["id"].as_i64().unwrap()
}

#[test]
fn card_move_transfers_across_boards() {
    let d = test_daemon(Config::default());
    let alpha = scoped_board(&d, "/alpha");
    let beta = scoped_board(&d, "/beta");
    let alpha_todo = handle_request(&d, "board.get", json!({ "board_id": alpha })).unwrap()
        ["columns"][0]["id"]
        .as_i64()
        .unwrap();
    let beta_done = handle_request(
        &d,
        "column.create",
        json!({ "board_id": beta, "name": "Done" }),
    )
    .unwrap()["id"]
        .as_i64()
        .unwrap();
    let card = handle_request(
        &d,
        "card.create",
        json!({ "board_id": alpha, "column_id": alpha_todo, "title": "ship" }),
    )
    .unwrap();
    let id = card["id"].as_i64().unwrap();
    assert_eq!(card["board_id"].as_i64(), Some(alpha));

    let moved = handle_request(
        &d,
        "card.move",
        json!({ "id": id, "board_id": beta, "column_id": beta_done }),
    )
    .unwrap();
    assert_eq!(moved["board_id"].as_i64(), Some(beta));
    assert_eq!(moved["column_id"].as_i64(), Some(beta_done));

    // The card is gone from alpha and present under beta.
    let alpha_snap = handle_request(&d, "board.get", json!({ "board_id": alpha })).unwrap();
    let beta_snap = handle_request(&d, "board.get", json!({ "board_id": beta })).unwrap();
    let in_alpha = alpha_snap["cards"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"].as_i64() == Some(id));
    let in_beta = beta_snap["cards"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"].as_i64() == Some(id));
    assert!(!in_alpha, "card must leave the source board");
    assert!(in_beta, "card must appear in the destination board");
}

#[test]
fn card_move_blocked_by_incompatible_target_harness() {
    let d = test_daemon(Config::default());
    let alpha = scoped_board(&d, "/alpha");
    let beta = scoped_board(&d, "/beta");
    let alpha_todo = handle_request(&d, "board.get", json!({ "board_id": alpha })).unwrap()
        ["columns"][0]["id"]
        .as_i64()
        .unwrap();
    // Destination column with an unknown harness override: the merged
    // capability check (reused from enqueue) must reject the move before any
    // mutation. Created directly at the DB layer (the op validates overrides
    // on column.create), so this column exists and is the incompatible target.
    let beta_done = {
        let db = d.store.lock();
        db.create_column(&ColumnCreateParams {
            board_id: Some(beta),
            name: "Done".into(),
            harness_override: Some("no-such-harness".into()),
            ..Default::default()
        })
        .unwrap()
        .id
    };
    let card = handle_request(
        &d,
        "card.create",
        json!({ "board_id": alpha, "column_id": alpha_todo, "title": "ship" }),
    )
    .unwrap();
    let id = card["id"].as_i64().unwrap();

    let err = handle_request(
        &d,
        "card.move",
        json!({ "id": id, "board_id": beta, "column_id": beta_done }),
    )
    .unwrap_err();
    // Capability validation surfaces as bad-request (code 1), same taxonomy as
    // enqueue; the message is what the TUI surfaces as a toast.
    assert_eq!(err.code(), 1, "expected validation error, got: {err}");
    assert!(err.to_string().to_lowercase().contains("harness"));

    // Fail-closed: nothing moved.
    let after = handle_request(&d, "card.get", json!({ "id": id })).unwrap();
    assert_eq!(after["card"]["board_id"].as_i64(), Some(alpha));
    assert_eq!(after["card"]["column_id"].as_i64(), Some(alpha_todo));
}

#[test]
fn card_move_blocked_when_session_cannot_resolve() {
    // A fake `herdr` that reports no sessions. A card pinned to a named session
    // ("ghost") then cannot resolve, so the blocking pre-check rejects the
    // move; the default-session card (session: null) is still allowed.
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("herdr");
    std::fs::write(&script, "#!/bin/sh\necho '{\"sessions\":[]}'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::env::set_var("HERDR_BIN_PATH", &script);

    let registry = Some(SessionRegistry::new(PathBuf::from("/tmp/board-test.sock")));
    let d = test_daemon_with_registry(Config::default(), registry);
    let alpha = scoped_board(&d, "/alpha");
    let beta = scoped_board(&d, "/beta");
    let alpha_todo = handle_request(&d, "board.get", json!({ "board_id": alpha })).unwrap()
        ["columns"][0]["id"]
        .as_i64()
        .unwrap();
    let beta_done = handle_request(
        &d,
        "column.create",
        json!({ "board_id": beta, "name": "Done" }),
    )
    .unwrap()["id"]
        .as_i64()
        .unwrap();

    // Named session that does not exist -> blocked.
    let ghost = handle_request(
        &d,
        "card.create",
        json!({ "board_id": alpha, "column_id": alpha_todo, "title": "g", "session": "ghost" }),
    )
    .unwrap()["id"]
        .as_i64()
        .unwrap();
    let err = handle_request(
        &d,
        "card.move",
        json!({ "id": ghost, "board_id": beta, "column_id": beta_done }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 3);
    assert!(err.to_string().to_lowercase().contains("session"));
    let after = handle_request(&d, "card.get", json!({ "id": ghost })).unwrap();
    assert_eq!(
        after["card"]["board_id"].as_i64(),
        Some(alpha),
        "blocked move must not move the card"
    );

    std::env::remove_var("HERDR_BIN_PATH");
}

#[test]
fn card_move_cross_board_emits_one_event_per_board() {
    let d = test_daemon(Config::default());
    let alpha = scoped_board(&d, "/alpha");
    let beta = scoped_board(&d, "/beta");
    let alpha_todo = handle_request(&d, "board.get", json!({ "board_id": alpha })).unwrap()
        ["columns"][0]["id"]
        .as_i64()
        .unwrap();
    let beta_done = handle_request(
        &d,
        "column.create",
        json!({ "board_id": beta, "name": "Done" }),
    )
    .unwrap()["id"]
        .as_i64()
        .unwrap();
    let id = handle_request(
        &d,
        "card.create",
        json!({ "board_id": alpha, "column_id": alpha_todo, "title": "x" }),
    )
    .unwrap()["id"]
        .as_i64()
        .unwrap();

    let mut rx = d.events_tx.subscribe();
    handle_request(
        &d,
        "card.move",
        json!({ "id": id, "board_id": beta, "column_id": beta_done }),
    )
    .unwrap();

    // A cross-board transfer emits one precise CardMoved per affected board,
    // each carrying its board_id (not two coarse, board-agnostic events).
    let mut board_ids = Vec::new();
    while let Ok(Event::BoardChanged {
        board_id, reason, ..
    }) = rx.try_recv()
    {
        if reason == BoardChangedReason::CardMoved {
            board_ids.push(board_id);
        }
    }
    assert!(
        board_ids.contains(&Some(alpha)),
        "source-board event missing: {board_ids:?}"
    );
    assert!(
        board_ids.contains(&Some(beta)),
        "destination-board event missing: {board_ids:?}"
    );
}
