use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_claims_global_fifo_heads_before_launch_and_serializes_competing_passes() {
    let spawner = Arc::new(PausedSpawner::default());
    let config = Config {
        max_concurrent: 2,
        ..Default::default()
    };
    let (d, _, _) = test_daemon_with_config(spawner.clone(), config);
    let (a1, a2, b1) = {
        let db = d.store.lock();
        let make = |title: &str, space_ref: &str| {
            db.create_card(&CardCreateParams {
                title: title.into(),
                space_kind: Some(SpaceKind::Workspace),
                space_ref: Some(space_ref.into()),
                ..Default::default()
            })
            .unwrap()
        };
        let a1 = make("A1", "space-a");
        let a2 = make("A2", "space-a");
        let b1 = make("B1", "space-b");
        for card in [&a1, &a2, &b1] {
            db.enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: card.title.as_str(),
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        }
        (a1, a2, b1)
    };

    // Deliberately race two callers. The per-daemon pass lock must keep the
    // second caller behind the first pass's pre-launch claims.
    let first = tokio::spawn({
        let d = d.clone();
        async move { dispatch_pass(&d).await }
    });
    let second = tokio::spawn({
        let d = d.clone();
        async move { dispatch_pass(&d).await }
    });

    let ready = tokio::time::timeout(Duration::from_secs(5), async {
        while spawner.started().len() < 2 {
            spawner.started_notify.notified().await;
        }
    })
    .await;
    let started = spawner.started();
    // Release blocking workers before asserting, including on RED. Otherwise
    // an assertion panic leaves the runtime waiting forever for these workers.
    spawner.release();
    first.await.unwrap();
    second.await.unwrap();
    ready.expect("two capacity claims must reach the spawner concurrently");
    assert_eq!(started.len(), 2, "global cap was exceeded: {started:?}");
    assert!(started
        .iter()
        .any(|name| name.starts_with(&format!("card-{}-", a1.id))));
    assert!(started
        .iter()
        .any(|name| name.starts_with(&format!("card-{}-", a2.id))));
    assert!(!started
        .iter()
        .any(|name| name.starts_with(&format!("card-{}-", b1.id))));

    let db = d.store.lock();
    let active_ids: Vec<_> = db
        .active_runs_with_cards()
        .unwrap()
        .into_iter()
        .map(|(_, card)| card.id)
        .collect();
    let queued_ids: Vec<_> = db
        .queued_runs_with_cards()
        .unwrap()
        .into_iter()
        .map(|(_, card)| card.id)
        .collect();
    assert_eq!(active_ids, vec![a1.id, a2.id]);
    assert_eq!(queued_ids, vec![b1.id]);
    assert_eq!(spawner.started().len(), 2);
}

fn queue_card(d: &Arc<Daemon>, board_id: i64, title: &str, workspace: &str) -> (Card, Run) {
    let card = d
        .store
        .lock()
        .create_card(&CardCreateParams {
            board_id: Some(board_id),
            title: title.into(),
            harness: Some("pi".into()),
            space_kind: Some(SpaceKind::Workspace),
            space_ref: Some(workspace.into()),
            space_cwd: Some(format!("/tasks/{title}")),
            ..Default::default()
        })
        .unwrap();
    let run = enqueue_run(d, card.id, card.column_id, false).unwrap();
    assert!(run.launch_spec.is_some());
    (card, run)
}

fn active_card_ids(d: &Arc<Daemon>) -> Vec<i64> {
    d.store
        .active_runs()
        .unwrap()
        .into_iter()
        .map(|(_, card)| card.id)
        .collect()
}

#[tokio::test]
async fn global_fifo_and_cap_span_boards_and_do_not_skip_a_shared_workspace() {
    let spawner = Arc::new(CapturingSpawner::default());
    let (d, _, _) = test_daemon_with_config(
        spawner.clone(),
        Config {
            max_concurrent: 2,
            ..Default::default()
        },
    );
    let other_board = d.store.lock().create_board(1, "other").unwrap();
    let (a, a_run) = queue_card(&d, 1, "first", "shared");
    let (b, _) = queue_card(&d, other_board.id, "second", "shared");
    let (c, _) = queue_card(&d, 1, "third", "elsewhere");

    dispatch_pass(&d).await;
    assert_eq!(active_card_ids(&d), vec![a.id, b.id]);
    assert_eq!(spawner.requests.lock().unwrap().len(), 2);
    // Repeated wakeups neither exceed the global cap nor launch an open card
    // twice. A new enqueue for that same card is still rejected by the UoW.
    assert!(enqueue_run(&d, a.id, a.column_id, false).is_err());
    dispatch_pass(&d).await;
    assert_eq!(spawner.requests.lock().unwrap().len(), 2);

    finalize_run(&d, a_run.id, RunOutcome::Ok, None, None, false, false).unwrap();
    dispatch_pass(&d).await;
    assert_eq!(active_card_ids(&d), vec![b.id, c.id]);
    assert_eq!(spawner.requests.lock().unwrap().len(), 3);
    for card in [a, b, c] {
        assert_eq!(d.store.lock().list_runs(card.id).unwrap().len(), 1);
    }
}

#[tokio::test]
async fn durable_active_runs_count_against_capacity_without_excluding_their_workspace() {
    let spawner = Arc::new(CapturingSpawner::default());
    let (d, _, _) = test_daemon_with_config(
        spawner.clone(),
        Config {
            max_concurrent: 2,
            ..Default::default()
        },
    );
    let (a, run) = queue_card(&d, 1, "recovered", "shared");
    d.store
        .lock()
        .promote_run_uow(run.id, Some("shared"), Some("shared:p1"), None)
        .unwrap();
    // Restart/Unknown reconciliation may leave no in-memory handle. The open
    // durable row must still occupy one slot, and must never be relaunched.
    assert!(d.sched.lock().unwrap().active.is_empty());
    let (b, _) = queue_card(&d, 1, "next", "shared");
    let (c, _) = queue_card(&d, 1, "later", "elsewhere");

    dispatch_pass(&d).await;
    dispatch_pass(&d).await;
    assert_eq!(active_card_ids(&d), vec![a.id, b.id]);
    let requests = spawner.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].name.starts_with(&format!("card-{}-", b.id)));
    let db = d.store.lock();
    let recovered = db.get_run(run.id).unwrap();
    assert_eq!(recovered.herdr_pane_id.as_deref(), Some("shared:p1"));
    assert_eq!(db.list_runs(a.id).unwrap().len(), 1);
    assert_eq!(
        db.get_card(c.id).unwrap().unwrap().status,
        CardStatus::Queued
    );
}

#[tokio::test]
async fn same_workspace_failure_releases_capacity_for_the_next_card_and_a_retry() {
    #[derive(Default)]
    struct FailFirst(AtomicUsize);

    impl Spawner for FailFirst {
        fn spawn(&self, _: &HerdrLaunchPlan) -> std::result::Result<RuntimeHandle, SpawnError> {
            if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(anyhow::anyhow!("injected launch failure").into())
            } else {
                Ok(RuntimeHandle {
                    pid: Some(4242),
                    ..Default::default()
                })
            }
        }

        fn kill(&self, _: &RuntimeHandle) -> anyhow::Result<()> {
            Ok(())
        }

        fn is_alive(&self, _: &RuntimeHandle) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    let spawner = Arc::new(FailFirst::default());
    let (d, _, _) = test_daemon_with_config(
        spawner.clone(),
        Config {
            max_concurrent: 2,
            ..Default::default()
        },
    );
    let (a, failed) = queue_card(&d, 1, "fails-once", "shared");
    dispatch_pass(&d).await;
    assert_eq!(
        d.store.lock().get_run(failed.id).unwrap().outcome,
        Some(RunOutcome::Fail)
    );
    let (b, _) = queue_card(&d, 1, "already-waiting", "shared");
    let retry = enqueue_run(&d, a.id, a.column_id, true).unwrap();
    assert!(retry.id > failed.id);

    dispatch_pass(&d).await;
    dispatch_pass(&d).await;
    assert_eq!(active_card_ids(&d), vec![b.id, a.id]);
    assert_eq!(spawner.0.load(Ordering::SeqCst), 3);
    assert_eq!(d.store.lock().list_runs(a.id).unwrap().len(), 2);
}
