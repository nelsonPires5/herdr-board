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
