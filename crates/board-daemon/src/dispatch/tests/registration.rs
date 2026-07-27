//! Spawn registration: the started row, card, and in-memory active
//! bookkeeping move together, and a handle whose row vanished is killed.

use super::*;

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
            anchor_pane_id: Some("anchor-41".into()),
            ..Default::default()
        },
        started,
        None,
        None,
    )
    .unwrap());

    let sched = d.sched.lock().unwrap();
    let db = d.store.lock();
    let promoted = db.get_run(run_id).unwrap();
    assert!(promoted.started_at.is_some());
    assert_eq!(promoted.herdr_anchor_pane_id.as_deref(), Some("anchor-41"));
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
