//! Fault-injection atomicity: every rolled-back promotion, spawn failure,
//! comment insert, and auto-hop enqueue must reopen the exact prior state and
//! let no event, dispatch wake, or kill escape.

use super::*;

#[tokio::test]
async fn promotion_fault_reopens_queued_state_without_started_effects_and_kills_handle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("promotion-fault.db");
    let (db, fault) = testkit::fault_db(
        &path,
        LifecycleFaultPoint::PromoteAfterRunUpdate,
        "injected promotion fault",
    );
    let card = db
        .create_card(&CardCreateParams {
            title: "promotion fault".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: "[]",
            prompt_snapshot: "prompt",
            system_prompt_snapshot: Some("system"),
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    let card_id = card.id;
    let run_id = run.id;
    let spawner = Arc::new(FaultPromotionSpawner::default());
    let (d, mut events_rx, mut dispatch_rx) = testkit::daemon()
        .db(db)
        .db_path(path.clone())
        .socket_path(dir.path().join("board.sock"))
        .spawner(spawner.clone())
        .build_parts();
    fault.arm();

    dispatch_pass(&d).await;

    assert_eq!(spawner.kills.load(Ordering::SeqCst), 1);
    assert!(!d.sched.lock().unwrap().active.contains_key(&run_id));
    let watch = d.watch.lock().unwrap();
    assert!(watch.panes_by_socket.is_empty());
    assert_eq!(watch.generation, 0);
    drop(watch);
    testkit::assert_no_effects(&mut events_rx, &mut dispatch_rx);

    drop(d);
    let reopened = Db::open(&path).unwrap();
    let card = reopened.get_card(card_id).unwrap().unwrap();
    let run = reopened.get_run(run_id).unwrap();
    assert_eq!(card.status, CardStatus::Queued);
    assert!(run.started_at.is_none());
    assert!(run.herdr_workspace_id.is_none());
    assert!(run.herdr_pane_id.is_none());
}

#[tokio::test]
async fn spawn_failure_finalization_is_atomic_and_uses_finalize_run_uow() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("spawn-fail-finalize.db");
    let (db, fault) = testkit::fault_db(
        &path,
        LifecycleFaultPoint::FinalizeAfterRunUpdate,
        "injected finalize fault",
    );
    let card = db
        .create_card(&CardCreateParams {
            title: "spawn fail finalize".into(),
            ..Default::default()
        })
        .unwrap();
    let run = db
        .enqueue_run_uow(&EnqueueRun {
            card_id: card.id,
            column_id: card.column_id,
            harness: "pi",
            argv_json: r#"["pi"]"#,
            prompt_snapshot: "prompt",
            system_prompt_snapshot: Some("system"),
            launch_spec_json: None,
            session_id: None,
            session: None,
        })
        .unwrap();
    let card_id = card.id;
    let run_id = run.id;

    // Capture exact queued card/run/comments before constructing the daemon.
    let captured_card = db.get_card(card_id).unwrap().unwrap();
    let captured_run = db.get_run(run_id).unwrap();
    let captured_comments = db.list_comments(card_id).unwrap();

    let spawner = Arc::new(MissingPiSpawner);
    let (d, mut events_rx, mut dispatch_rx) = testkit::daemon()
        .db(db)
        .db_path(path.clone())
        .socket_path(dir.path().join("board.sock"))
        .spawner(spawner)
        .build_parts();

    // Arm the fault point only before dispatch.
    fault.arm();

    dispatch_pass(&d).await;

    // The hook must have been observed.
    assert!(
        fault.observed(),
        "FinalizeAfterRunUpdate hook was never observed – fail_queued_run bypasses finalize_run_uow"
    );

    // No terminal event or dispatch wake escaped.
    testkit::assert_no_effects(&mut events_rx, &mut dispatch_rx);

    // Reopen DB must exactly equal captured state.
    drop(d);
    let reopened = Db::open(&path).unwrap();
    let card = reopened.get_card(card_id).unwrap().unwrap();
    let run = reopened.get_run(run_id).unwrap();
    let comments = reopened.list_comments(card_id).unwrap();
    assert_eq!(card, captured_card);
    assert_eq!(run, captured_run);
    assert_eq!(comments, captured_comments);
}

#[test]
fn finalization_planning_error_preserves_exact_prior_state_and_emits_nothing() {
    let (d, mut events, mut dispatch) = test_daemon_with_receivers(Arc::new(MissingPiSpawner));
    let (card_id, run_id, target_id) = {
        let db = d.store.lock();
        let source = db
            .create_column(&ColumnCreateParams {
                name: "Source".into(),
                ..Default::default()
            })
            .unwrap();
        let target = db
            .create_column(&ColumnCreateParams {
                name: "Target".into(),
                trigger: Some(Trigger::Auto),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: source.id,
            on_success_column_id: Patch::Set(target.id),
            ..Default::default()
        })
        .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                column_id: Some(source.id),
                title: "bad next harness".into(),
                harness: Some("missing".into()),
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
        db.promote_run_uow(run.id, None, None, None).unwrap();
        db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
            .unwrap();
        (card.id, run.id, target.id)
    };

    let err = finalize_run(&d, run_id, RunOutcome::Ok, None, None, false, true).unwrap_err();
    assert!(err.to_string().contains("unknown harness"));

    let db = d.store.lock();
    let run = db.get_run(run_id).unwrap();
    let card = db.get_card(card_id).unwrap().unwrap();
    assert!(run.ended_at.is_none());
    assert_eq!(run.outcome, None);
    assert_ne!(card.column_id, target_id);
    assert_eq!(card.status, CardStatus::Awaiting);
    assert_eq!(card.awaiting_reason, Some(AwaitingReason::AgentDone));
    assert_eq!(db.list_runs(card_id).unwrap().len(), 1);
    assert!(db.list_comments(card_id).unwrap().is_empty());
    drop(db);
    testkit::assert_no_effects(&mut events, &mut dispatch);
}

fn file_daemon(
    db: Db,
    path: PathBuf,
    spawner: Arc<dyn Spawner>,
) -> (
    Arc<Daemon>,
    broadcast::Receiver<Event>,
    mpsc::UnboundedReceiver<()>,
) {
    testkit::daemon()
        .db(db)
        .db_path(path)
        .socket_path(PathBuf::from("/tmp/board-finalize-test.sock"))
        .spawner(spawner)
        .events_capacity(32)
        .build_parts()
}

#[test]
fn daemon_comment_insert_fault_reopens_exact_prior_state_without_precommit_effects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("comment-fault.db");
    let db = Db::open(&path).unwrap();
    let (card_id, run_id, column_id) = {
        let card = db
            .create_card(&CardCreateParams {
                title: "comment rollback".into(),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "prompt",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("workspace"), Some("pane"), None)
            .unwrap();
        db.set_card_awaiting(card.id, AwaitingReason::AgentDone)
            .unwrap();
        db.add_comment(card.id, "user", "durable before").unwrap();
        (card.id, run.id, card.column_id)
    };
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER abort_daemon_comment BEFORE INSERT ON comments
             BEGIN SELECT RAISE(ABORT, 'injected daemon comment failure'); END;",
        )
        .unwrap();
    let spawner = Arc::new(RecordingSpawner::default());
    let (d, mut events, mut dispatch) = file_daemon(db, path.clone(), spawner.clone());
    let effects = Arc::new(Mutex::new(Vec::new()));
    *d.effect_log.lock().unwrap() = Some(effects.clone());
    d.sched.lock().unwrap().active.insert(
        run_id,
        ActiveRun {
            card_id,
            handle: RuntimeHandle {
                pane_id: Some("pane".into()),
                ..Default::default()
            },
            started: Instant::now(),
            timeout_deadline: None,
            idle_since: None,
            awaiting_since: Some(Instant::now()),
            is_local: false,
            pane_id: Some("pane".into()),
        },
    );

    let err = finalize_run(
        &d,
        run_id,
        RunOutcome::Cancelled,
        Some("must roll back".into()),
        Some("must not persist".into()),
        true,
        true,
    )
    .unwrap_err();
    assert!(err.to_string().contains("injected daemon comment failure"));
    testkit::assert_no_rollback_effects(&d, &mut events, &mut dispatch, &spawner.kills, run_id);
    assert!(effects.lock().unwrap().is_empty());
    drop(d);

    let reopened = Db::open(&path).unwrap();
    let run = reopened.get_run(run_id).unwrap();
    let card = reopened.get_card(card_id).unwrap().unwrap();
    assert!(run.ended_at.is_none());
    assert_eq!(run.outcome, None);
    assert_eq!(run.result_summary, None);
    assert_eq!(card.column_id, column_id);
    assert_eq!(card.status, CardStatus::Awaiting);
    assert_eq!(card.awaiting_reason, Some(AwaitingReason::AgentDone));
    let comments = reopened.list_comments(card_id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].author, "user");
    assert_eq!(comments[0].body, "durable before");
    assert_eq!(reopened.list_runs(card_id).unwrap().len(), 1);
}

#[test]
fn daemon_auto_hop_enqueue_fault_reopens_exact_prior_state_without_precommit_effects() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("auto-hop-fault.db");
    let db = Db::open(&path).unwrap();
    let (card_id, run_id, source_id) = {
        let source = db
            .create_column(&ColumnCreateParams {
                name: "Fault source".into(),
                ..Default::default()
            })
            .unwrap();
        let target = db
            .create_column(&ColumnCreateParams {
                name: "Fault auto target".into(),
                trigger: Some(Trigger::Auto),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: source.id,
            on_success_column_id: Patch::Set(target.id),
            ..Default::default()
        })
        .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "auto hop rollback".into(),
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
                prompt_snapshot: "prompt",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, Some("workspace"), Some("pane"), None)
            .unwrap();
        db.add_comment(card.id, "user", "durable before").unwrap();
        (card.id, run.id, source.id)
    };
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch(&format!(
            "CREATE TRIGGER abort_daemon_next BEFORE INSERT ON runs
             WHEN NEW.card_id={card_id}
             BEGIN SELECT RAISE(ABORT, 'injected daemon next enqueue failure'); END;"
        ))
        .unwrap();
    let spawner = Arc::new(RecordingSpawner::default());
    let (d, mut events, mut dispatch) = file_daemon(db, path.clone(), spawner.clone());
    let effects = Arc::new(Mutex::new(Vec::new()));
    *d.effect_log.lock().unwrap() = Some(effects.clone());
    d.sched.lock().unwrap().active.insert(
        run_id,
        ActiveRun {
            card_id,
            handle: RuntimeHandle {
                pane_id: Some("pane".into()),
                ..Default::default()
            },
            started: Instant::now(),
            timeout_deadline: None,
            idle_since: None,
            awaiting_since: None,
            is_local: false,
            pane_id: Some("pane".into()),
        },
    );

    let err = finalize_run(
        &d,
        run_id,
        RunOutcome::Ok,
        Some("must roll back".into()),
        Some("must not persist".into()),
        true,
        true,
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("injected daemon next enqueue failure"));
    testkit::assert_no_rollback_effects(&d, &mut events, &mut dispatch, &spawner.kills, run_id);
    assert!(effects.lock().unwrap().is_empty());
    assert_eq!(d.sched.lock().unwrap().chain_hops.get(&card_id), None);
    drop(d);

    let reopened = Db::open(&path).unwrap();
    let run = reopened.get_run(run_id).unwrap();
    let card = reopened.get_card(card_id).unwrap().unwrap();
    assert!(run.ended_at.is_none());
    assert_eq!(run.outcome, None);
    assert_eq!(run.result_summary, None);
    assert_eq!(card.column_id, source_id);
    assert_eq!(card.status, CardStatus::Running);
    assert_eq!(card.awaiting_reason, None);
    let comments = reopened.list_comments(card_id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].body, "durable before");
    assert_eq!(reopened.list_runs(card_id).unwrap().len(), 1);
}
