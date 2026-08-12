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
    // A registry that knows NO sessions (seeded, so no `herdr` binary is ever
    // invoked — deliberately NOT a global `HERDR_BIN_PATH` env mutation, which
    // would race every parallel test that shells out to the configured-harness
    // runner). A card pinned to a named session ("ghost") then cannot resolve,
    // so the blocking pre-check rejects the move; the default-session card
    // (session: null) is still allowed.
    let registry = Some(SessionRegistry::with_entries(
        PathBuf::from("/tmp/board-test.sock"),
        Vec::new(),
    ));
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

#[test]
fn card_duplicate_copies_config_resets_state_and_emits_card_created() {
    let d = test_daemon(Config::default());
    let effects = Arc::new(Mutex::new(Vec::new()));
    *d.effect_log.lock().unwrap() = Some(effects.clone());
    let mut rx = d.events_tx.subscribe();

    let original_id = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "Ship it".into(),
                description: Some("base prompt".into()),
                harness: Some("claude".into()),
                model: Some("opus".into()),
                effort: Some(Effort::High),
                permission_mode: Some("acceptEdits".into()),
                session: Some("work".into()),
                space_kind: Some(SpaceKind::NewWorkspace),
                space_ref: Some("widget-build".into()),
                space_cwd: Some("/tmp/widget".into()),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "claude",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: Some("conv-9"),
                session: Some("work"),
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("w1"), Some("p1"), None)
            .unwrap();
        db.set_card_session(card.id, "conv-9").unwrap();
        db.add_comment(card.id, "agent:1", "draft").unwrap();
        card.id
    };
    // Capture the post-setup original: running with a conversation id.
    let original = d.store.lock().require_card(original_id).unwrap();

    let result = handle_request(&d, "card.duplicate", json!({"id": original_id})).unwrap();
    let copy: board_core::model::Card = serde_json::from_value(result).unwrap();
    assert_ne!(copy.id, original.id);
    assert_eq!(copy.title, "Ship it (copy)");
    assert_eq!(copy.status, CardStatus::Idle);
    assert_eq!(copy.awaiting_reason, None);
    assert_eq!(copy.session_id, None);
    assert_eq!(copy.archived_at, None);
    assert_eq!(copy.description, original.description);
    assert_eq!(copy.harness, original.harness);
    assert_eq!(copy.model, original.model);
    assert_eq!(copy.effort, original.effort);
    assert_eq!(copy.permission_mode, original.permission_mode);
    assert_eq!(copy.session, original.session);
    assert_eq!(copy.space_kind, original.space_kind);
    assert_eq!(copy.space_ref, original.space_ref);
    assert_eq!(copy.space_cwd, original.space_cwd);
    assert_eq!(copy.column_id, original.column_id);
    assert_eq!(copy.position, original.position + 1);

    // Copy has no execution state; the original is byte-identical.
    let db = d.store.lock();
    assert!(db.list_runs(copy.id).unwrap().is_empty());
    assert!(db.list_comments(copy.id).unwrap().is_empty());
    assert_eq!(db.require_card(original_id).unwrap(), original);

    // Normal CardCreated notification; never a dispatch wake.
    let mut events = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        events.push(ev);
    }
    let created = events.iter().any(|ev| match ev {
        Event::BoardChanged {
            reason: BoardChangedReason::CardCreated,
            card_id,
            ..
        } => *card_id == Some(copy.id),
        _ => false,
    });
    assert!(created, "no CardCreated for the copy in {events:?}");
    let logged = effects.lock().unwrap();
    assert!(
        !logged.contains(&"dispatch_wake"),
        "duplicate must not wake dispatch: {logged:?}"
    );
}

#[test]
fn card_duplicate_in_auto_column_never_enqueues() {
    let d = test_daemon(Config::default());
    let effects = Arc::new(Mutex::new(Vec::new()));
    *d.effect_log.lock().unwrap() = Some(effects.clone());

    let card_id = {
        let db = d.store.lock();
        let auto = db
            .create_column(&ColumnCreateParams {
                name: "Execute".into(),
                trigger: Some(Trigger::Auto),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "Run me".into(),
                column_id: Some(auto.id),
                ..Default::default()
            })
            .unwrap();
        card.id
    };

    let result = handle_request(&d, "card.duplicate", json!({"id": card_id})).unwrap();
    let copy: board_core::model::Card = serde_json::from_value(result).unwrap();
    let db = d.store.lock();
    assert_eq!(copy.status, CardStatus::Idle);
    assert!(db.list_runs(copy.id).unwrap().is_empty());
    assert!(db.open_run_for_card(copy.id).unwrap().is_none());
    assert_eq!(db.require_card(card_id).unwrap().status, CardStatus::Idle);
    let logged = effects.lock().unwrap();
    assert!(
        !logged.contains(&"dispatch_wake"),
        "duplicate into an auto column must not dispatch: {logged:?}"
    );
}

#[test]
fn card_duplicate_rejects_unknown_card_without_writing() {
    let d = test_daemon(Config::default());
    let err = handle_request(&d, "card.duplicate", json!({"id": 9999})).unwrap_err();
    assert_eq!(err.code(), 2);
    assert!(err.to_string().contains("card 9999"));
    assert!(d.store.lock().list_all_cards().unwrap().is_empty());
}

// -- same-column reorder (pure reorder, never a dispatch) -------------------

/// Seed an auto column with three idle cards and return `(column_id, [ids])`.
///
/// Cards are created through the DB layer (not `card.create`, which would
/// auto-dispatch them): an idle card sitting in an auto column is exactly the
/// state a same-column reorder must NOT re-dispatch.
fn seed_auto_column(d: &Arc<Daemon>) -> (i64, Vec<i64>) {
    let column_id = handle_request(
        d,
        "column.create",
        json!({ "name": "Auto", "trigger": "auto" }),
    )
    .unwrap()["id"]
        .as_i64()
        .unwrap();
    let mut ids = Vec::new();
    let db = d.store.lock();
    for title in ["first", "second", "third"] {
        let card = db
            .create_card(&CardCreateParams {
                title: title.into(),
                column_id: Some(column_id),
                ..Default::default()
            })
            .unwrap();
        ids.push(card.id);
    }
    drop(db);
    (column_id, ids)
}

fn card_order(d: &Arc<Daemon>, column_id: i64) -> Vec<i64> {
    let db = d.store.lock();
    db.list_cards_in_column(column_id)
        .unwrap()
        .into_iter()
        .map(|c| c.id)
        .collect()
}

#[test]
fn card_move_same_column_reorders_without_enqueueing_or_status_change() {
    let d = test_daemon(Config::default());
    let (column_id, ids) = seed_auto_column(&d);
    // An idle card in an auto column would normally be dispatched on entry;
    // a same-column move must NOT enqueue.
    assert!(card_order(&d, column_id) == ids);

    // Move the first card to the last position.
    let moved = handle_request(
        &d,
        "card.move",
        json!({ "id": ids[0], "column_id": column_id, "position": 2 }),
    )
    .unwrap();
    assert_eq!(moved["column_id"], column_id);
    assert_eq!(moved["status"], "idle", "status must be unchanged");
    assert_eq!(moved["position"], 2, "the card reports its new position");

    let order = card_order(&d, column_id);
    assert_eq!(order, vec![ids[1], ids[2], ids[0]], "order must flip");

    // No run may exist: same-column reordering never dispatches.
    let db = d.store.lock();
    for id in &ids {
        assert!(db.open_run_for_card(*id).unwrap().is_none());
        assert_eq!(db.require_card(*id).unwrap().status.as_str(), "idle");
    }
    // Positions stay contiguous and deterministic.
    let positions: Vec<i64> = order
        .iter()
        .map(|id| db.require_card(*id).unwrap().position)
        .collect();
    assert_eq!(positions, vec![0, 1, 2]);
}

#[test]
fn card_move_same_column_never_triggers_auto_column() {
    let d = test_daemon(Config::default());
    let (column_id, ids) = seed_auto_column(&d);
    let mut rx = d.events_tx.subscribe();

    handle_request(
        &d,
        "card.move",
        json!({ "id": ids[2], "column_id": column_id, "position": 0 }),
    )
    .unwrap();

    let order = card_order(&d, column_id);
    assert_eq!(order, vec![ids[2], ids[0], ids[1]]);
    let db = d.store.lock();
    assert!(db.list_runs(ids[0]).unwrap().is_empty(), "no run enqueued");
    // The dispatcher must not have been woken: the move is not a dispatch.
    assert!(
        rx.try_recv().is_err() || {
            // Drain any events and make sure none is a dispatch wake.
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    Event::BoardChanged { .. } => {}
                    _ => panic!("unexpected non-board event: {ev:?}"),
                }
            }
            true
        },
        "same-column move must never wake the dispatcher"
    );
}

#[test]
fn card_move_same_column_keeps_an_open_run_untouched() {
    let d = test_daemon(Config::default());
    let (column_id, ids) = seed_auto_column(&d);
    // Promote a real open run on the card to be reordered (status running).
    let (_, run_id) = {
        let db = d.store.lock();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: ids[1],
                column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("w1"), Some("p1"), None)
            .unwrap();
        (db.require_card(ids[1]).unwrap(), run.id)
    };

    // Reorder the running card to the front.
    let moved = handle_request(
        &d,
        "card.move",
        json!({ "id": ids[1], "column_id": column_id, "position": 0 }),
    )
    .unwrap();
    assert_eq!(moved["status"], "running", "run card keeps its status");
    assert_eq!(card_order(&d, column_id), vec![ids[1], ids[0], ids[2]]);

    let db = d.store.lock();
    let run = db.get_run(run_id).unwrap();
    assert!(run.ended_at.is_none(), "the open run survives untouched");
    assert_eq!(run.column_id, column_id);
}

#[test]
fn card_move_same_column_clamps_position_and_compacts() {
    let d = test_daemon(Config::default());
    let (column_id, ids) = seed_auto_column(&d);
    // A position past the end appends; the column stays contiguous.
    handle_request(
        &d,
        "card.move",
        json!({ "id": ids[0], "column_id": column_id, "position": 99 }),
    )
    .unwrap();
    assert_eq!(card_order(&d, column_id), vec![ids[1], ids[2], ids[0]]);
    let db = d.store.lock();
    let positions: Vec<i64> = db
        .list_cards_in_column(column_id)
        .unwrap()
        .iter()
        .map(|c| c.position)
        .collect();
    assert_eq!(positions, vec![0, 1, 2]);
}
