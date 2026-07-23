use super::*;

#[tokio::test]
async fn promotion_fault_reopens_queued_state_without_started_effects_and_kills_handle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("promotion-fault.db");
    let armed = Arc::new(AtomicBool::new(false));
    let fault_armed = armed.clone();
    let db = Db::open_with_lifecycle_fault_hook(&path, move |point| {
        if fault_armed.load(Ordering::SeqCst) && point == LifecycleFaultPoint::PromoteAfterRunUpdate
        {
            return Err(Error::InvalidState("injected promotion fault".into()));
        }
        Ok(())
    })
    .unwrap();
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
    let (events_tx, mut events_rx) = broadcast::channel(16);
    let (dispatch_tx, mut dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let d = Arc::new(Daemon::new(
        Store::new(db),
        Config::default(),
        DaemonSettings::default(),
        path.clone(),
        dir.path().join("board.sock"),
        spawner.clone(),
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));
    armed.store(true, Ordering::SeqCst);

    dispatch_pass(&d).await;

    assert_eq!(spawner.kills.load(Ordering::SeqCst), 1);
    assert!(!d.sched.lock().unwrap().active.contains_key(&run_id));
    let watch = d.watch.lock().unwrap();
    assert!(watch.panes_by_socket.is_empty());
    assert_eq!(watch.generation, 0);
    drop(watch);
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
    let armed = Arc::new(AtomicBool::new(false));
    let hook_observed = Arc::new(AtomicBool::new(false));
    let fault_armed = armed.clone();
    let fault_observed = hook_observed.clone();
    let db = Db::open_with_lifecycle_fault_hook(&path, move |point| {
        if fault_armed.load(Ordering::SeqCst)
            && point == LifecycleFaultPoint::FinalizeAfterRunUpdate
        {
            fault_observed.store(true, Ordering::SeqCst);
            return Err(Error::InvalidState("injected finalize fault".into()));
        }
        Ok(())
    })
    .unwrap();
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
    let (events_tx, mut events_rx) = broadcast::channel(16);
    let (dispatch_tx, mut dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let d = Arc::new(Daemon::new(
        Store::new(db),
        Config::default(),
        DaemonSettings::default(),
        path.clone(),
        dir.path().join("board.sock"),
        spawner,
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));

    // Arm the fault point only before dispatch.
    armed.store(true, Ordering::SeqCst);

    dispatch_pass(&d).await;

    // The hook must have been observed.
    assert!(
        hook_observed.load(Ordering::SeqCst),
        "FinalizeAfterRunUpdate hook was never observed – fail_queued_run bypasses finalize_run_uow"
    );

    // No terminal event or dispatch wake escaped.
    assert!(matches!(
        events_rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        dispatch_rx.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    ));

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
fn spawned_run_registration_starts_row_card_and_active_bookkeeping_together() {
    let spawner = Arc::new(RecordingSpawner::default());
    let d = test_daemon(spawner.clone());
    let (card_id, run_id) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "register atomically".into(),
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
        (card.id, run.id)
    };
    let started = Instant::now();

    assert!(register_spawned_run(
        &d,
        run_id,
        RuntimeHandle {
            pid: Some(41),
            ..Default::default()
        },
        started,
        None,
        None,
    )
    .unwrap());

    let sched = d.sched.lock().unwrap();
    let db = d.store.lock();
    assert!(db.get_run(run_id).unwrap().started_at.is_some());
    assert_eq!(
        db.get_card(card_id).unwrap().unwrap().status,
        CardStatus::Running
    );
    assert_eq!(sched.active.get(&run_id).unwrap().handle.pid, Some(41));
    assert_eq!(spawner.kills.load(Ordering::SeqCst), 0);
}

#[test]
fn spawned_run_registration_kills_handle_when_row_was_cancelled() {
    let spawner = Arc::new(RecordingSpawner::default());
    let d = test_daemon(spawner.clone());
    let (card_id, run_id) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "cancelled during spawn".into(),
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
        db.finalize_run_uow(&FinalizeRun {
            run_id: run.id,
            outcome: RunOutcome::Cancelled,
            summary: Some("cancelled"),
            comments: &[],
            target_column_id: None,
            final_status: CardStatus::Failed,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();
        (card.id, run.id)
    };

    assert!(!register_spawned_run(
        &d,
        run_id,
        RuntimeHandle {
            pid: Some(42),
            ..Default::default()
        },
        Instant::now(),
        None,
        None,
    )
    .unwrap());

    let db = d.store.lock();
    let run = db.get_run(run_id).unwrap();
    assert!(run.started_at.is_none());
    assert_eq!(run.outcome, Some(RunOutcome::Cancelled));
    assert_eq!(
        db.get_card(card_id).unwrap().unwrap().status,
        CardStatus::Failed
    );
    drop(db);
    assert!(!d.sched.lock().unwrap().active.contains_key(&run_id));
    assert_eq!(spawner.kills.load(Ordering::SeqCst), 1);
}

#[test]
fn auto_transition_enqueues_once_inside_finalization_transaction() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card_id, run_id, target_id) = {
        let db = d.store.lock();
        let source = db
            .create_column(&ColumnCreateParams {
                name: "Source".into(),
                trigger: Some(Trigger::Auto),
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
                title: "chain".into(),
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
        (card.id, run.id, target.id)
    };

    let (_, card) = finalize_run(&d, run_id, RunOutcome::Ok, None, None, false, true).unwrap();

    assert_eq!(card.column_id, target_id);
    assert_eq!(card.status, CardStatus::Queued);
    let runs = d.store.lock().list_runs(card_id).unwrap();
    assert_eq!(runs.len(), 2);
    assert_eq!(runs.iter().filter(|run| run.ended_at.is_none()).count(), 1);
    let next = runs.iter().find(|run| run.ended_at.is_none()).unwrap();
    assert!(
        next.launch_spec.is_some(),
        "auto-hop must materialize exactly one v11 spec"
    );
    assert_eq!(next.session, card.session);
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
    assert!(events.try_recv().is_err());
    assert!(dispatch.try_recv().is_err());
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
    let (events_tx, events_rx) = broadcast::channel(32);
    let (dispatch_tx, dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let daemon = Arc::new(Daemon::new(
        Store::new(db),
        Config::default(),
        DaemonSettings::default(),
        path,
        PathBuf::from("/tmp/board-finalize-test.sock"),
        spawner,
        None,
        None,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ));
    (daemon, events_rx, dispatch_rx)
}

fn assert_no_effects(
    d: &Arc<Daemon>,
    events: &mut broadcast::Receiver<Event>,
    dispatch: &mut mpsc::UnboundedReceiver<()>,
    spawner: &RecordingSpawner,
    run_id: i64,
) {
    assert_eq!(spawner.kills.load(Ordering::SeqCst), 0);
    assert!(d.sched.lock().unwrap().active.contains_key(&run_id));
    assert!(
        events.try_recv().is_err(),
        "terminal event escaped rollback"
    );
    assert!(
        dispatch.try_recv().is_err(),
        "dispatch wake escaped rollback"
    );
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
    assert_no_effects(&d, &mut events, &mut dispatch, &spawner, run_id);
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
    assert_no_effects(&d, &mut events, &mut dispatch, &spawner, run_id);
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

#[derive(Clone, Copy, Debug)]
enum TerminalPath {
    BoardDone,
    Cancel,
    Timeout,
    PaneExit,
}

fn invoke_terminal_path(d: &Arc<Daemon>, run_id: i64, path: TerminalPath) -> Result<(Run, Card)> {
    match path {
        TerminalPath::BoardDone => finalize_run(
            d,
            run_id,
            RunOutcome::Ok,
            Some("board done".into()),
            None,
            false,
            true,
        ),
        TerminalPath::Cancel => finalize_run(
            d,
            run_id,
            RunOutcome::Cancelled,
            Some("cancel".into()),
            None,
            true,
            false,
        ),
        TerminalPath::Timeout => finalize_run_timeout(
            d,
            run_id,
            Instant::now(),
            RunOutcome::Fail,
            Some("timeout".into()),
            Some("timeout".into()),
            true,
            true,
        )?
        .ok_or_else(|| Error::InvalidState("timeout lost".into())),
        TerminalPath::PaneExit => finalize_run(
            d,
            run_id,
            RunOutcome::Fail,
            Some("pane exit".into()),
            Some("pane exit".into()),
            false,
            false,
        ),
    }
}

#[test]
fn terminal_winner_duplicate_and_stale_matrix_is_idempotent() {
    let paths = [
        TerminalPath::BoardDone,
        TerminalPath::Cancel,
        TerminalPath::Timeout,
        TerminalPath::PaneExit,
    ];
    for winner in paths {
        for loser in paths {
            let spawner = Arc::new(RecordingSpawner::default());
            let (d, mut events, mut dispatch) = test_daemon_with_receivers(spawner.clone());
            let (card_id, run_id) = {
                let db = d.store.lock();
                let card = db
                    .create_card(&CardCreateParams {
                        title: format!("winner {winner:?}, loser {loser:?}"),
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
                (card.id, run.id)
            };
            d.sched.lock().unwrap().active.insert(
                run_id,
                ActiveRun {
                    card_id,
                    handle: RuntimeHandle {
                        pane_id: Some("pane".into()),
                        ..Default::default()
                    },
                    started: Instant::now(),
                    timeout_deadline: Some(Instant::now() - Duration::from_secs(1)),
                    idle_since: None,
                    awaiting_since: None,
                    is_local: false,
                    pane_id: Some("pane".into()),
                },
            );

            let (won_run, won_card) = invoke_terminal_path(&d, run_id, winner).unwrap();
            let won_outcome = won_run.outcome;
            let won_status = won_card.status;
            let won_column = won_card.column_id;
            let won_comments = d.store.lock().list_comments(card_id).unwrap();
            while events.try_recv().is_ok() {}
            while dispatch.try_recv().is_ok() {}
            let kills = spawner.kills.load(Ordering::SeqCst);

            let duplicate = invoke_terminal_path(&d, run_id, loser).unwrap();
            assert_eq!(duplicate.0.outcome, won_outcome, "{winner:?} vs {loser:?}");
            assert_eq!(duplicate.1.status, won_status, "{winner:?} vs {loser:?}");
            assert!(events.try_recv().is_err());
            assert!(dispatch.try_recv().is_err());
            assert_eq!(spawner.kills.load(Ordering::SeqCst), kills);
            assert_eq!(d.store.lock().list_comments(card_id).unwrap(), won_comments);

            let replacement = enqueue_run(&d, card_id, won_column, true).unwrap();
            while events.try_recv().is_ok() {}
            while dispatch.try_recv().is_ok() {}
            let stale = invoke_terminal_path(&d, run_id, loser).unwrap();
            assert_eq!(
                stale.0.outcome, won_outcome,
                "stale {winner:?} vs {loser:?}"
            );
            assert_eq!(spawner.kills.load(Ordering::SeqCst), kills);
            assert!(events.try_recv().is_err());
            assert!(dispatch.try_recv().is_err());
            let db = d.store.lock();
            let replacement = db.get_run(replacement.id).unwrap();
            assert!(replacement.ended_at.is_none());
            assert_eq!(
                db.get_card(card_id).unwrap().unwrap().status,
                CardStatus::Queued
            );
            assert_eq!(db.list_comments(card_id).unwrap(), won_comments);
        }
    }
}

#[test]
fn successful_finalization_records_exact_postcommit_effect_order() {
    let spawner = Arc::new(RecordingSpawner::default());
    let (d, _events, _dispatch) = test_daemon_with_receivers(spawner.clone());
    let (card_id, run_id) = {
        let db = d.store.lock();
        let source = db
            .create_column(&ColumnCreateParams {
                name: "effect source".into(),
                ..Default::default()
            })
            .unwrap();
        let review = db
            .create_column(&ColumnCreateParams {
                name: "Review".into(),
                trigger: Some(Trigger::Manual),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: source.id,
            on_success_column_id: Patch::Set(review.id),
            ..Default::default()
        })
        .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "ordered effects".into(),
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
        (card.id, run.id)
    };
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
    let effects = Arc::new(Mutex::new(Vec::new()));
    *d.effect_log.lock().unwrap() = Some(effects.clone());
    *spawner.effects.lock().unwrap() = Some(effects.clone());

    finalize_run(&d, run_id, RunOutcome::Ok, None, None, true, true).unwrap();

    assert_eq!(
        *effects.lock().unwrap(),
        [
            "scheduler",
            "watch",
            "kill",
            "notification",
            "run_ended",
            "board_changed",
            "dispatch_wake"
        ]
    );
}

#[tokio::test]
async fn spawn_failure_for_missing_pi_marks_run_failed_with_system_comment() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card_id, column_id) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "missing pi".into(),
                ..Default::default()
            })
            .unwrap();
        (card.id, card.column_id)
    };
    let run = enqueue_run(&d, card_id, column_id, false).unwrap();

    dispatch_pass(&d).await;

    let db = d.store.lock();
    let finished = db.get_run(run.id).unwrap();
    assert_eq!(finished.outcome, Some(RunOutcome::Fail));
    assert_eq!(
        db.get_card(card_id).unwrap().unwrap().status,
        CardStatus::Failed
    );
    assert!(db
        .list_comments(card_id)
        .unwrap()
        .iter()
        .any(|comment| comment.author == "system"
            && comment.body.contains("spawn failed")
            && comment.body.contains("pi not found")));
}

#[test]
fn scoped_run_transition_uses_the_cards_board_columns() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card, run, target) = {
        let db = d.store.lock();
        let board = db.open_board("/scoped").unwrap();
        let auto = db
            .create_column(&ColumnCreateParams {
                board_id: Some(board.id),
                name: "Execute".into(),
                trigger: Some(Trigger::Auto),
                ..Default::default()
            })
            .unwrap();
        let done = db
            .create_column(&ColumnCreateParams {
                board_id: Some(board.id),
                name: "Done".into(),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: auto.id,
            on_success_column_id: Patch::Set(done.id),
            ..Default::default()
        })
        .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                board_id: Some(board.id),
                column_id: Some(auto.id),
                title: "scoped transition".into(),
                ..Default::default()
            })
            .unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: auto.id,
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
        (card, run, done)
    };

    let (_, moved) = finalize_run(&d, run.id, RunOutcome::Ok, None, None, false, true).unwrap();
    assert_eq!(moved.board_id, card.board_id);
    assert_eq!(moved.column_id, target.id);
}
