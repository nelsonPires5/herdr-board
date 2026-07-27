//! Terminal-path idempotency and the finalization transaction: exactly one
//! winner across `board done` / cancel / timeout / pane-exit, the auto-hop
//! enqueue inside the same transaction, and the exact post-commit effect order.

use super::*;

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
            testkit::assert_no_effects(&mut events, &mut dispatch);
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
            testkit::assert_no_effects(&mut events, &mut dispatch);
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
