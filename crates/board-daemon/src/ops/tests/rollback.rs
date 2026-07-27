//! Request-level fault rollback: an injected lifecycle fault must reopen the
//! exact prior state and let no event or dispatch wake escape.

use super::*;

#[test]
fn enqueue_fault_reopens_prior_state_without_event_or_dispatch_wake() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enqueue-fault.db");
    let (db, fault) = testkit::fault_db(
        &path,
        LifecycleFaultPoint::EnqueueAfterRunInsert,
        "injected enqueue fault",
    );
    let card = db
        .create_card(&CardCreateParams {
            title: "enqueue fault".into(),
            ..Default::default()
        })
        .unwrap();
    let card_id = card.id;
    let (d, mut events_rx, mut dispatch_rx) = testkit::daemon()
        .db(db)
        .db_path(path.clone())
        .socket_path(dir.path().join("board.sock"))
        .build_parts();
    fault.arm();

    let err = handle_request(&d, "run.retry", json!({"card_id": card_id})).unwrap_err();
    assert!(err.to_string().contains("injected enqueue fault"));
    testkit::assert_no_effects(&mut events_rx, &mut dispatch_rx);

    drop(d);
    let reopened = Db::open(&path).unwrap();
    let card = reopened.get_card(card_id).unwrap().unwrap();
    assert_eq!(card.status, CardStatus::Idle);
    assert_eq!(card.session_id, None);
    assert!(reopened.list_runs(card_id).unwrap().is_empty());
}

#[test]
fn cancel_queued_fault_reopens_prior_state_without_event_or_dispatch_wake() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cancel-queued-fault.db");
    let (db, fault) = testkit::fault_db(
        &path,
        LifecycleFaultPoint::FinalizeAfterRunUpdate,
        "injected finalize fault",
    );
    let card = db
        .create_card(&CardCreateParams {
            title: "cancel queued fault".into(),
            ..Default::default()
        })
        .unwrap();
    let card_id = card.id;
    let column_id = card.column_id;
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id,
            column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "test",
            system_prompt_snapshot: None,
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    let run_id = run.id;
    let queued_card = db.get_card(card_id).unwrap().unwrap();
    let comments = db.list_comments(card_id).unwrap();
    let (d, mut events_rx, mut dispatch_rx) = testkit::daemon()
        .db(db)
        .db_path(path.clone())
        .socket_path(dir.path().join("board.sock"))
        .build_parts();
    fault.arm();

    let err = handle_request(&d, "run.cancel", json!({"card_id": card_id})).unwrap_err();
    assert!(err.to_string().contains("injected finalize fault"));
    testkit::assert_no_effects(&mut events_rx, &mut dispatch_rx);

    drop(d);
    let reopened = Db::open(&path).unwrap();
    assert_eq!(reopened.get_card(card_id).unwrap().unwrap(), queued_card);
    assert_eq!(reopened.get_run(run_id).unwrap(), run);
    assert_eq!(reopened.list_comments(card_id).unwrap(), comments);
}

#[test]
fn card_create_into_auto_rolls_back_when_enqueue_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("create-auto-fault.db");
    let (db, fault) = testkit::fault_db(
        &path,
        LifecycleFaultPoint::EnqueueAfterRunInsert,
        "injected enqueue fault",
    );
    let auto = db
        .create_column(&ColumnCreateParams {
            name: "Auto".into(),
            trigger: Some(Trigger::Auto),
            ..Default::default()
        })
        .unwrap();
    let before_columns = db.list_columns(BOARD_ID).unwrap();
    let (d, mut events_rx, mut dispatch_rx) =
        testkit::daemon().db(db).db_path(path.clone()).build_parts();
    fault.arm();

    let error = handle_request(
        &d,
        "card.create",
        json!({"column_id":auto.id, "title":"must not persist"}),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("injected enqueue fault"),
        "{error}"
    );

    testkit::assert_no_effects(&mut events_rx, &mut dispatch_rx);

    drop(d);
    let reopened = Db::open(&path).unwrap();
    assert_eq!(reopened.list_columns(BOARD_ID).unwrap(), before_columns);
    assert!(reopened.list_cards(BOARD_ID).unwrap().is_empty());
}

#[test]
fn card_move_into_auto_rolls_back_when_enqueue_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("move-auto-fault.db");
    let (db, fault) = testkit::fault_db(
        &path,
        LifecycleFaultPoint::EnqueueAfterRunInsert,
        "injected enqueue fault",
    );
    let source = db.default_column_id(BOARD_ID).unwrap();
    let auto = db
        .create_column(&ColumnCreateParams {
            name: "Auto".into(),
            trigger: Some(Trigger::Auto),
            ..Default::default()
        })
        .unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: "must stay put".into(),
            ..Default::default()
        })
        .unwrap();
    let before = card.clone();
    let (d, mut events_rx, mut dispatch_rx) =
        testkit::daemon().db(db).db_path(path.clone()).build_parts();
    fault.arm();

    let error =
        handle_request(&d, "card.move", json!({"id":card.id, "column_id":auto.id})).unwrap_err();
    assert!(
        error.to_string().contains("injected enqueue fault"),
        "{error}"
    );

    testkit::assert_no_effects(&mut events_rx, &mut dispatch_rx);

    drop(d);
    let reopened = Db::open(&path).unwrap();
    assert_eq!(reopened.get_card(card.id).unwrap().unwrap(), before);
    assert!(reopened.list_runs(card.id).unwrap().is_empty());
    assert_eq!(reopened.list_cards_in_column(source).unwrap().len(), 1);
    assert!(reopened.list_cards_in_column(auto.id).unwrap().is_empty());
}
