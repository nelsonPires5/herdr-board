use super::*;

#[test]
fn enqueue_fault_reopens_prior_state_without_event_or_dispatch_wake() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("enqueue-fault.db");
    let armed = Arc::new(AtomicBool::new(false));
    let fault_armed = armed.clone();
    let db = Db::open_with_lifecycle_fault_hook(&path, move |point| {
        if fault_armed.load(Ordering::SeqCst) && point == LifecycleFaultPoint::EnqueueAfterRunInsert
        {
            return Err(Error::InvalidState("injected enqueue fault".into()));
        }
        Ok(())
    })
    .unwrap();
    let card = db
        .create_card(&CardCreateParams {
            title: "enqueue fault".into(),
            ..Default::default()
        })
        .unwrap();
    let card_id = card.id;
    let (events_tx, mut events_rx) = broadcast::channel(16);
    let (dispatch_tx, mut dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let d = Arc::new(Daemon::new(
        Store::new(db),
        Config::default(),
        DaemonSettings::default(),
        path.clone(),
        dir.path().join("board.sock"),
        Arc::new(LocalSpawner::new()),
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));
    armed.store(true, Ordering::SeqCst);

    let err = handle_request(&d, "run.retry", json!({"card_id": card_id})).unwrap_err();
    assert!(err.to_string().contains("injected enqueue fault"));
    assert!(matches!(
        events_rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        dispatch_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

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
    let armed = Arc::new(AtomicBool::new(false));
    let fault_armed = armed.clone();
    let db = Db::open_with_lifecycle_fault_hook(&path, move |point| {
        if fault_armed.load(Ordering::SeqCst)
            && point == LifecycleFaultPoint::FinalizeAfterRunUpdate
        {
            return Err(Error::InvalidState("injected finalize fault".into()));
        }
        Ok(())
    })
    .unwrap();
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
    let (events_tx, mut events_rx) = broadcast::channel(16);
    let (dispatch_tx, mut dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let d = Arc::new(Daemon::new(
        Store::new(db),
        Config::default(),
        DaemonSettings::default(),
        path.clone(),
        dir.path().join("board.sock"),
        Arc::new(LocalSpawner::new()),
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));
    armed.store(true, Ordering::SeqCst);

    let err = handle_request(&d, "run.cancel", json!({"card_id": card_id})).unwrap_err();
    assert!(err.to_string().contains("injected finalize fault"));
    assert!(matches!(
        events_rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        dispatch_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

    drop(d);
    let reopened = Db::open(&path).unwrap();
    assert_eq!(reopened.get_card(card_id).unwrap().unwrap(), queued_card);
    assert_eq!(reopened.get_run(run_id).unwrap(), run);
    assert_eq!(reopened.list_comments(card_id).unwrap(), comments);
}

#[test]
fn run_done_ok_without_target_column_marks_card_done() {
    let d = test_daemon(Config::default());
    let (card_id, run_id) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "confirm me".into(),
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
        db.promote_run_uow(run.id, Some("w1"), Some("p1"), None)
            .unwrap();
        // Simulate the pre-confirmation state: awaiting human review.
        db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
            .unwrap();
        (card.id, run.id)
    };

    let res = handle_request(&d, "run.done", json!({"card_id": card_id, "outcome": "ok"})).unwrap();

    // The seed Todo column has no on_success target: confirmed completion
    // lands on `done` (not `idle`), clearing the awaiting reason.
    assert_eq!(res["run"]["id"], run_id);
    assert_eq!(res["run"]["outcome"], "ok");
    assert_eq!(res["card"]["status"], "done");
    assert!(res["card"]["awaiting_reason"].is_null());
}

#[test]
fn run_done_accepts_matching_queued_configured_run_before_pane_registration() {
    let mut config = Config::default();
    config.harness.insert(
        "custom".into(),
        HarnessDef {
            argv: vec!["custom-agent".into()],
            ..Default::default()
        },
    );
    let d = test_daemon(config);
    let (card_id, run_id, target_id) = {
        let db = d.store.lock();
        let target = db
            .create_column(&ColumnCreateParams {
                name: "pre-registration target".into(),
                ..Default::default()
            })
            .unwrap();
        let source = db
            .create_column(&ColumnCreateParams {
                name: "pre-registration source".into(),
                trigger: Some(Trigger::Auto),
                on_success_column_id: Some(target.id),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "done before pane registration".into(),
                column_id: Some(source.id),
                harness: Some("custom".into()),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: source.id,
                harness: "custom",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        // The configured runner can report board done before the daemon
        // registers the spawned pane, so this is an open queued run.
        (card.id, run.id, target.id)
    };

    let result = handle_request(
        &d,
        "run.done",
        json!({
            "card_id": card_id,
            "run_id": run_id,
            "outcome": "ok",
            "summary": "completed before pane registration"
        }),
    )
    .unwrap();

    assert_eq!(result["run"]["id"], run_id);
    assert_eq!(result["run"]["outcome"], "ok");
    assert_eq!(
        result["run"]["result_summary"],
        "completed before pane registration"
    );
    assert_eq!(result["card"]["column_id"], target_id);
    assert_eq!(result["card"]["status"], "idle");

    // A late pane-exit callback must not turn the already-successful run
    // into a configured-harness failure.
    let pane_exit = handle_request(
        &d,
        "run.pane_exited",
        json!({"card_id": card_id, "run_id": run_id}),
    )
    .unwrap_err();
    assert!(pane_exit.to_string().contains("no open run"), "{pane_exit}");
    let db = d.store.lock();
    let run = db.get_run(run_id).unwrap();
    assert_eq!(run.outcome, Some(RunOutcome::Ok));
    assert!(run.ended_at.is_some());
    let card = db.get_card(card_id).unwrap().unwrap();
    assert_eq!(card.column_id, target_id);
    assert_eq!(card.status, CardStatus::Idle);
}

#[test]
fn run_done_rejects_queued_configured_runs_without_or_with_stale_run_id() {
    for stale in [false, true] {
        let mut config = Config::default();
        config.harness.insert(
            "custom".into(),
            HarnessDef {
                argv: vec!["custom-agent".into()],
                ..Default::default()
            },
        );
        let d = test_daemon(config);
        let (card_id, run_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: "queued callback identity".into(),
                    harness: Some("custom".into()),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .enqueue_run_uow(&EnqueueRun {
                    card_id: card.id,
                    column_id: card.column_id,
                    harness: "custom",
                    argv_json: "[]",
                    prompt_snapshot: "p",
                    system_prompt_snapshot: None,
                    launch_spec_json: None,
                    session_id: None,
                    session: None,
                })
                .unwrap();
            (card.id, run.id)
        };

        let params = if stale {
            json!({"card_id": card_id, "run_id": run_id + 1, "outcome": "ok"})
        } else {
            json!({"card_id": card_id, "outcome": "ok"})
        };
        let err = handle_request(&d, "run.done", params).unwrap_err();
        assert!(err.to_string().contains("no active run"), "{err}");

        let db = d.store.lock();
        let run = db.get_run(run_id).unwrap();
        assert!(run.ended_at.is_none());
        assert!(run.outcome.is_none());
        assert_eq!(
            db.get_card(card_id).unwrap().unwrap().status,
            CardStatus::Queued
        );
    }
}

#[test]
fn run_done_rejects_mismatching_run_id_for_a_different_active_replacement() {
    let mut config = Config::default();
    config.harness.insert(
        "custom".into(),
        HarnessDef {
            argv: vec!["custom-agent".into()],
            ..Default::default()
        },
    );
    let d = test_daemon(config);
    let (card_id, stale_run_id, active_run_id) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "replacement callback identity".into(),
                harness: Some("custom".into()),
                ..Default::default()
            })
            .unwrap();
        let stale = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "custom",
                argv_json: "[]",
                prompt_snapshot: "old",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(stale.id, Some("w1"), Some("old-pane"), None)
            .unwrap();
        db.finalize_run_uow(&FinalizeRun {
            run_id: stale.id,
            outcome: RunOutcome::Fail,
            summary: Some("replaced"),
            comments: &[],
            target_column_id: None,
            final_status: CardStatus::Failed,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();

        let active = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "custom",
                argv_json: "[]",
                prompt_snapshot: "new",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(active.id, Some("w1"), Some("new-pane"), None)
            .unwrap();
        (card.id, stale.id, active.id)
    };

    let err = handle_request(
        &d,
        "run.done",
        json!({
            "card_id": card_id,
            "run_id": stale_run_id,
            "outcome": "ok"
        }),
    )
    .unwrap_err();
    assert!(err.to_string().contains("run"), "{err}");

    let db = d.store.lock();
    assert_eq!(
        db.get_run(stale_run_id).unwrap().outcome,
        Some(RunOutcome::Fail)
    );
    assert!(db.get_run(active_run_id).unwrap().ended_at.is_none());
    assert_eq!(
        db.get_card(card_id).unwrap().unwrap().status,
        CardStatus::Running
    );
}

#[test]
fn run_done_rejects_queued_builtin_runs_before_pane_registration() {
    for harness in ["pi", "claude"] {
        let d = test_daemon(Config::default());
        let (card_id, run_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: format!("queued builtin {harness}"),
                    harness: Some(harness.into()),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .enqueue_run_uow(&EnqueueRun {
                    card_id: card.id,
                    column_id: card.column_id,
                    harness,
                    argv_json: "[]",
                    prompt_snapshot: "p",
                    system_prompt_snapshot: None,
                    launch_spec_json: None,
                    session_id: None,
                    session: None,
                })
                .unwrap();
            (card.id, run.id)
        };

        let err = handle_request(
            &d,
            "run.done",
            json!({"card_id": card_id, "run_id": run_id, "outcome": "ok"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no active run"), "{err}");

        let db = d.store.lock();
        let run = db.get_run(run_id).unwrap();
        assert!(run.ended_at.is_none());
        assert!(run.outcome.is_none());
        assert_eq!(
            db.get_card(card_id).unwrap().unwrap().status,
            CardStatus::Queued
        );
    }
}

#[test]
fn pane_exited_finalizes_matching_run_without_on_fail_transition() {
    let d = test_daemon(Config::default());
    let (card_id, run_id, source_id) = {
        let db = d.store.lock();
        let target = db
            .create_column(&ColumnCreateParams {
                name: "pane-exit target".into(),
                ..Default::default()
            })
            .unwrap();
        let source = db
            .create_column(&ColumnCreateParams {
                name: "pane-exit source".into(),
                trigger: Some(Trigger::Auto),
                on_fail_column_id: Some(target.id),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "silent configured harness".into(),
                column_id: Some(source.id),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: source.id,
                harness: "fake",
                argv_json: "[]",
                prompt_snapshot: "silent",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("w1"), Some("p1"), None)
            .unwrap();
        (card.id, run.id, source.id)
    };

    let stale = handle_request(
        &d,
        "run.pane_exited",
        json!({"card_id": card_id, "run_id": run_id + 1}),
    )
    .unwrap_err();
    assert!(stale.to_string().contains("run"));
    {
        let db = d.store.lock();
        assert!(db.get_run(run_id).unwrap().ended_at.is_none());
        let card = db.get_card(card_id).unwrap().unwrap();
        assert_eq!(card.status, CardStatus::Running);
        assert_eq!(card.column_id, source_id);
    }

    let res = handle_request(
        &d,
        "run.pane_exited",
        json!({"card_id": card_id, "run_id": run_id}),
    )
    .unwrap();
    assert_eq!(res["run"]["outcome"], "fail");
    assert_eq!(
        res["run"]["result_summary"],
        "configured harness exited without calling board done"
    );
    assert_eq!(res["card"]["status"], "failed");
    assert_eq!(res["card"]["column_id"], source_id);
    let detail = handle_request(&d, "card.get", json!({"id": card_id})).unwrap();
    assert!(detail["comments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|comment| comment["body"] == "pane exited without board done"));
}

#[test]
fn pane_exited_accepts_matching_queued_configured_run() {
    let d = test_daemon(Config::default());
    let (card_id, run_id, column_id) = {
        let db = d.store.lock();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "queued configured".into(),
                on_fail_column_id: Some(db.default_column_id(BOARD_ID).unwrap()),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "callback before registration".into(),
                column_id: Some(column.id),
                harness: Some("custom".into()),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: column.id,
                harness: "custom",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        (card.id, run.id, column.id)
    };

    let result = handle_request(
        &d,
        "run.pane_exited",
        json!({"card_id": card_id, "run_id": run_id}),
    )
    .unwrap();

    assert_eq!(result["run"]["id"], run_id);
    assert_eq!(result["run"]["outcome"], "fail");
    assert_eq!(result["card"]["status"], "failed");
    assert_eq!(result["card"]["column_id"], column_id);
    let detail = handle_request(&d, "card.get", json!({"id": card_id})).unwrap();
    assert!(detail["comments"]
        .as_array()
        .unwrap()
        .iter()
        .any(|comment| { comment["body"] == "pane exited without board done" }));
}

#[test]
fn pane_exited_rejects_builtin_runs_without_mutating_them() {
    for harness in ["pi", "claude"] {
        let d = test_daemon(Config::default());
        let (card_id, run_id) = {
            let db = d.store.lock();
            let card = db
                .create_card(&CardCreateParams {
                    title: format!("builtin {harness}"),
                    harness: Some(harness.into()),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .enqueue_run_uow(&EnqueueRun {
                    card_id: card.id,
                    column_id: card.column_id,
                    harness,
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
            (card.id, run.id)
        };

        let err = handle_request(
            &d,
            "run.pane_exited",
            json!({"card_id": card_id, "run_id": run_id}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("configured harness"), "{err}");

        let db = d.store.lock();
        let run = db.get_run(run_id).unwrap();
        assert!(run.ended_at.is_none());
        assert!(run.outcome.is_none());
        assert_eq!(
            db.get_card(card_id).unwrap().unwrap().status,
            CardStatus::Running
        );
        assert!(db.list_comments(card_id).unwrap().is_empty());
    }
}

#[test]
fn run_retry_rejects_every_kind_of_open_run_from_db_truth() {
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
        let err = handle_request(&d, "run.retry", json!({"card_id": card_id})).unwrap_err();
        assert_eq!(err.code(), 3);
        assert!(err.to_string().contains("open run"));
    }
}

#[test]
fn column_delete_rejects_queued_blocked_and_awaiting_open_runs() {
    for (status, started) in [
        (CardStatus::Queued, false),
        (CardStatus::Blocked, true),
        (CardStatus::Awaiting, true),
    ] {
        let d = test_daemon(Config::default());
        let (source_id, target_id) = {
            let db = d.store.lock();
            let target_id = db.default_column_id(BOARD_ID).unwrap();
            let source = db
                .create_column(&ColumnCreateParams {
                    name: "Source".into(),
                    ..Default::default()
                })
                .unwrap();
            let card = db
                .create_card(&CardCreateParams {
                    title: format!("{status:?}"),
                    column_id: Some(source.id),
                    ..Default::default()
                })
                .unwrap();
            let run = db
                .enqueue_run_uow(&EnqueueRun {
                    card_id: card.id,
                    column_id: source.id,
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
            (source.id, target_id)
        };

        let err = handle_request(
            &d,
            "column.delete",
            json!({"id": source_id, "move_cards_to": target_id}),
        )
        .unwrap_err();
        assert_eq!(err.code(), 3);
        assert!(err.to_string().contains("open run"));
    }
}
