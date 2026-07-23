use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_claims_a1_and_b1_before_launch_and_serializes_competing_passes() {
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

    while spawner.started().len() < 2 {
        spawner.started_notify.notified().await;
    }
    let started = spawner.started();
    assert_eq!(started.len(), 2, "global cap was exceeded: {started:?}");
    assert!(started
        .iter()
        .any(|name| name.starts_with(&format!("card-{}-", a1.id))));
    assert!(started
        .iter()
        .any(|name| name.starts_with(&format!("card-{}-", b1.id))));
    assert!(!started
        .iter()
        .any(|name| name.starts_with(&format!("card-{}-", a2.id))));

    spawner.release();
    first.await.unwrap();
    second.await.unwrap();

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
    assert_eq!(active_ids, vec![a1.id, b1.id]);
    assert_eq!(queued_ids, vec![a2.id]);
    assert_eq!(spawner.started().len(), 2);
}
