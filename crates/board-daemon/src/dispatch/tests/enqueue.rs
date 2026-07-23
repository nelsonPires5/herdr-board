use super::*;

#[test]
fn pi_is_builtin_and_does_not_receive_custom_prompt_env() {
    assert!(harness_prompt_env("pi", "prompt", Some("system")).is_empty());
    assert!(harness_prompt_env("claude", "prompt", Some("system")).is_empty());
    assert_eq!(
        harness_prompt_env("fake", "prompt", Some("system")),
        vec![
            ("BOARD_PROMPT".into(), "prompt".into()),
            (
                "BOARD_SYSTEM_PROMPT".into(),
                board_core::harness::protocol_system_prompt(Some("system")),
            ),
        ]
    );
    // No column prompt → the trailer alone, never a missing env var.
    assert_eq!(
        harness_prompt_env("fake", "prompt", None),
        vec![
            ("BOARD_PROMPT".into(), "prompt".into()),
            (
                "BOARD_SYSTEM_PROMPT".into(),
                board_core::harness::protocol_system_prompt(None),
            ),
        ]
    );
}

#[test]
fn pi_fork_persists_the_new_target_session_id() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card_id, column_id, old_session) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "retry".into(),
                harness: Some("pi".into()),
                effort: Some(Effort::Low),
                ..Default::default()
            })
            .unwrap();
        let old_session = "11111111-1111-4111-8111-111111111111";
        db.set_card_session(card.id, old_session).unwrap();
        let prior = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "pi",
                argv_json: "[]",
                prompt_snapshot: "prior",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: Some(old_session),
                session: None,
            })
            .unwrap();
        db.promote_run_uow(prior.id, None, None, None).unwrap();
        let prior_id = prior.id;
        db.finalize_run_uow(&FinalizeRun {
            run_id: prior_id,
            outcome: RunOutcome::Ok,
            summary: None,
            comments: &[(&format!("agent:{}", prior_id), "done")],
            target_column_id: None,
            final_status: CardStatus::Done,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();
        (card.id, card.column_id, old_session.to_string())
    };

    let run = enqueue_run(&d, card_id, column_id, true).unwrap();
    let card = d.store.lock().get_card(card_id).unwrap().unwrap();
    let new_session = card.session_id.unwrap();
    assert_ne!(new_session, old_session);
    assert_eq!(run.session_id.as_deref(), Some(new_session.as_str()));
    assert!(run.launch_spec.is_some());
    assert_eq!(
        run.launch_spec.as_ref().unwrap().execution().argv,
        serde_json::from_str::<Vec<String>>(&run.argv_json).unwrap()
    );
    let argv: Vec<String> = serde_json::from_str(&run.argv_json).unwrap();
    assert!(argv
        .windows(2)
        .any(|w| w == ["--fork", old_session.as_str()]));
    assert!(argv
        .windows(2)
        .any(|w| w == ["--session-id", new_session.as_str()]));
}

#[test]
fn enqueue_run_final_guard_prevents_duplicate_open_runs() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card_id, column_id) = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                title: "single open run".into(),
                ..Default::default()
            })
            .unwrap();
        (card.id, card.column_id)
    };

    let first = enqueue_run(&d, card_id, column_id, true).unwrap();
    let err = enqueue_run(&d, card_id, column_id, true).unwrap_err();
    assert_eq!(err.code(), 3);
    assert!(err.to_string().contains("open run"));
    let open_runs: Vec<_> = d
        .store
        .lock()
        .list_runs(card_id)
        .unwrap()
        .into_iter()
        .filter(|run| run.ended_at.is_none())
        .collect();
    assert_eq!(open_runs.len(), 1);
    assert_eq!(open_runs[0].id, first.id);
}

#[derive(Debug, PartialEq, Eq)]
struct EnqueueSnapshotSpec {
    harness: String,
    model: Option<String>,
    effort: Option<Effort>,
    permission_mode: Option<String>,
    system_prompt: Option<String>,
    fresh_session: bool,
    prompt: String,
    session: Option<String>,
}

// Test-only seam for the authoritative-lock contract: production enqueue
// must call the pure snapshot builders again from the locked state rather
// than persist the values prepared before the lock.
fn authoritative_enqueue_snapshot(
    card: &board_core::model::Card,
    column: &board_core::model::Column,
    comments: &[board_core::model::Comment],
) -> EnqueueSnapshotSpec {
    let settings = effective_settings(card, column).unwrap();
    EnqueueSnapshotSpec {
        harness: settings.harness,
        model: settings.model,
        effort: settings.effort,
        permission_mode: settings.permission_mode,
        system_prompt: settings.system_prompt,
        fresh_session: settings.fresh_session,
        prompt: assemble_prompt(&card.description, comments),
        session: card.session.clone(),
    }
}

#[test]
fn enqueue_snapshot_spec_rebuilds_after_authoritative_card_changes() {
    let d = test_daemon(Arc::new(MissingPiSpawner));
    let (card_id, column_id) = {
        let db = d.store.lock();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "authoritative old".into(),
                system_prompt: Some("old settings".into()),
                model_override: Some("old-model".into()),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "authoritative snapshot".into(),
                column_id: Some(column.id),
                harness: Some("pi".into()),
                description: Some("old prompt".into()),
                session: Some("old-herdr-session".into()),
                ..Default::default()
            })
            .unwrap();
        db.add_comment(card.id, "user", "old comment").unwrap();
        (card.id, column.id)
    };

    let prepared = {
        let db = d.store.lock();
        authoritative_enqueue_snapshot(
            &db.get_card(card_id).unwrap().unwrap(),
            &db.get_column(column_id).unwrap().unwrap(),
            &db.list_comments(card_id).unwrap(),
        )
    };

    {
        let db = d.store.lock();
        db.update_card(&CardUpdateParams {
            id: card_id,
            description: Some("new prompt".into()),
            model: Patch::Set("new-model".into()),
            session: Patch::Set("new-herdr-session".into()),
            ..Default::default()
        })
        .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: column_id,
            system_prompt: Patch::Set("new settings".into()),
            model_override: Patch::Set("new-column-model".into()),
            ..Default::default()
        })
        .unwrap();
        db.add_comment(card_id, "user", "new comment").unwrap();
    }

    let rebuilt = {
        let db = d.store.lock();
        authoritative_enqueue_snapshot(
            &db.get_card(card_id).unwrap().unwrap(),
            &db.get_column(column_id).unwrap().unwrap(),
            &db.list_comments(card_id).unwrap(),
        )
    };
    assert_ne!(prepared, rebuilt);
    assert_eq!(rebuilt.harness, "pi");
    assert_eq!(rebuilt.model.as_deref(), Some("new-column-model"));
    assert_eq!(rebuilt.system_prompt.as_deref(), Some("new settings"));
    assert_eq!(rebuilt.session.as_deref(), Some("new-herdr-session"));
    assert!(rebuilt.prompt.contains("new prompt"));
    assert!(rebuilt.prompt.contains("new comment"));
    assert!(!rebuilt.prompt.contains("old prompt"));
    // Existing comments remain part of the authoritative current list;
    // the new comment must not be dropped while rebuilding.
    assert!(rebuilt.prompt.contains("old comment"));
}

#[tokio::test]
async fn queued_managed_pi_uses_enqueue_time_system_snapshot() {
    let spawner = Arc::new(CapturingSpawner::default());
    let d = test_daemon(spawner.clone());
    let (card_id, column_id) = {
        let db = d.store.lock();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "Execute".into(),
                trigger: Some(Trigger::Auto),
                system_prompt: Some("old column instructions".into()),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "snapshot dispatch".into(),
                column_id: Some(column.id),
                harness: Some("pi".into()),
                description: Some("task body".into()),
                ..Default::default()
            })
            .unwrap();
        (card.id, column.id)
    };
    let run = enqueue_run(&d, card_id, column_id, false).unwrap();
    let exact = run.launch_spec.as_ref().unwrap().execution().clone();
    let old = board_core::harness::protocol_system_prompt(Some("old column instructions"));
    d.store
        .lock()
        .update_card(&CardUpdateParams {
            id: card_id,
            description: Some("edited task must not launch".into()),
            model: Patch::Set("edited-model".into()),
            ..Default::default()
        })
        .unwrap();
    d.store
        .lock()
        .update_column(&ColumnUpdateParams {
            id: column_id,
            system_prompt: Patch::Set("new column instructions".into()),
            ..Default::default()
        })
        .unwrap();

    dispatch_pass(&d).await;

    let requests = spawner.requests.lock().unwrap();
    let req = &requests[0];
    assert_eq!(req.argv, exact.argv);
    assert_eq!(req.agent_kind, exact.agent_kind);
    assert_eq!(req.initial_prompt, exact.initial_prompt);
    assert_eq!(req.system_prompt, exact.system_prompt);
    assert_eq!(req.agent_kind.as_deref(), Some("pi"));
    assert_eq!(
        req.initial_prompt.as_deref(),
        Some(run.prompt_snapshot.as_str())
    );
    assert_eq!(req.system_prompt.as_deref(), Some(old.as_str()));
    assert!(req
        .argv
        .iter()
        .all(|arg| !arg.contains("old column instructions")));
    assert!(req.argv.iter().all(|arg| !arg.contains("task body")));
}

#[tokio::test]
async fn queued_configured_harness_uses_enqueue_time_system_snapshot() {
    let spawner = Arc::new(CapturingSpawner::default());
    let mut d = test_daemon(spawner.clone());
    Arc::get_mut(&mut d).unwrap().config.harness.insert(
        "custom".into(),
        board_core::config::HarnessDef {
            argv: vec!["custom-agent".into()],
            ..Default::default()
        },
    );
    let (card_id, column_id) = {
        let db = d.store.lock();
        let column = db
            .create_column(&ColumnCreateParams {
                name: "Configured".into(),
                system_prompt: Some("configured old".into()),
                ..Default::default()
            })
            .unwrap();
        let card = db
            .create_card(&CardCreateParams {
                title: "configured snapshot".into(),
                column_id: Some(column.id),
                harness: Some("custom".into()),
                description: Some("configured task".into()),
                ..Default::default()
            })
            .unwrap();
        (card.id, column.id)
    };
    let run = enqueue_run(&d, card_id, column_id, false).unwrap();
    let exact = run.launch_spec.as_ref().unwrap().execution().clone();
    Arc::get_mut(&mut d)
        .unwrap()
        .config
        .harness
        .get_mut("custom")
        .unwrap()
        .argv = vec!["edited-agent-must-not-launch".into()];
    d.store
        .lock()
        .update_card(&CardUpdateParams {
            id: card_id,
            description: Some("edited configured task".into()),
            ..Default::default()
        })
        .unwrap();
    d.store
        .lock()
        .update_column(&ColumnUpdateParams {
            id: column_id,
            system_prompt: Patch::Set("configured new".into()),
            ..Default::default()
        })
        .unwrap();
    dispatch_pass(&d).await;
    let requests = spawner.requests.lock().unwrap();
    let req = &requests[0];
    assert_eq!(req.argv, exact.argv);
    assert_eq!(req.agent_kind, exact.agent_kind);
    assert_eq!(req.initial_prompt, exact.initial_prompt);
    assert_eq!(req.system_prompt, exact.system_prompt);
    assert_eq!(&req.env[..exact.env.len()], exact.env.as_slice());
    assert_eq!(req.env.len(), exact.env.len() + 4);
    let env = &req.env;
    assert_eq!(
        env.iter()
            .find(|(k, _)| k == "BOARD_SYSTEM_PROMPT")
            .unwrap()
            .1,
        board_core::harness::protocol_system_prompt(Some("configured old"))
    );
    assert_eq!(
        env.iter().find(|(k, _)| k == "BOARD_BIN").map(|(_, v)| v),
        Some(
            &std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        )
    );
}

// ---------------------------------------------------------------------------
// T13: manual enqueue vs auto-hop → identical persisted EnqueueRun fields
// ---------------------------------------------------------------------------

/// `prepare_enqueue_values` produces the same persisted `EnqueueRun` fields
/// whether called from the manual `enqueue_run` path or from the auto-hop
/// path inside `finalize_run`. The comparison excludes the random session id
/// (a fresh UUID minted on each call) as well as row ids and timestamps.
/// Transition-generated comments must not contaminate the auto-hop
/// preparation: `prepare_enqueue_values` reads DB comments, and the
/// transition comment is only persisted inside the same `finalize_run_uow`
/// *after* the next-enqueue values have been prepared.
#[test]
fn prepare_enqueue_values_is_deterministic_for_equivalent_inputs() {
    let spawner = Arc::new(MissingPiSpawner);
    let mut d = test_daemon(spawner.clone());
    Arc::get_mut(&mut d).unwrap().config.harness.insert(
        "custom-det".into(),
        board_core::config::HarnessDef {
            argv: vec!["custom-det-agent".into()],
            ..Default::default()
        },
    );

    // Columns with identical settings so both paths resolve the same
    // effective configuration. Source on_success → Target.
    let (source_id, target_id) = {
        let db = d.store.lock();
        let source = db
            .create_column(&ColumnCreateParams {
                name: "Src".into(),
                trigger: Some(Trigger::Auto),
                system_prompt: Some("col-sys".into()),
                model_override: Some("col-model".into()),
                ..Default::default()
            })
            .unwrap();
        let target = db
            .create_column(&ColumnCreateParams {
                name: "Tgt".into(),
                trigger: Some(Trigger::Auto),
                system_prompt: Some("col-sys".into()),
                model_override: Some("col-model".into()),
                ..Default::default()
            })
            .unwrap();
        db.update_column(&ColumnUpdateParams {
            id: source.id,
            on_success_column_id: Patch::Set(target.id),
            ..Default::default()
        })
        .unwrap();
        (source.id, target.id)
    };

    // One card, first used for the manual path (in target column), then
    // moved to source for the auto-hop path.
    let card_id = {
        let db = d.store.lock();
        let card = db
            .create_card(&CardCreateParams {
                column_id: Some(target_id),
                title: "det".into(),
                harness: Some("custom-det".into()),
                description: Some("task body".into()),
                session: Some("herdr-session".into()),
                ..Default::default()
            })
            .unwrap();
        db.add_comment(card.id, "user", "user note").unwrap();
        card.id
    };

    // --- Manual path: enqueue directly to target column ---
    let manual = enqueue_run(&d, card_id, target_id, false).unwrap();

    // Complete the manual run quietly (no extra comments) so the card can be
    // reused for the auto-hop path without contaminating the comment list.
    {
        let db = d.store.lock();
        let open = db.open_run_for_card(card_id).unwrap().unwrap();
        db.promote_run_uow(open.id, None, None, None).unwrap();
        db.finalize_run_uow(&FinalizeRun {
            run_id: open.id,
            outcome: RunOutcome::Ok,
            summary: None,
            comments: &[],
            target_column_id: None,
            final_status: CardStatus::Done,
            final_awaiting_reason: None,
            next: None,
        })
        .unwrap();
    }

    // --- Auto-hop path: move card to source, enqueue a dummy run, finalize ---
    d.store.lock().move_card(card_id, source_id, None).unwrap();
    let src_run = {
        let db = d.store.lock();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id,
                column_id: source_id,
                harness: "custom-det",
                argv_json: "[]",
                prompt_snapshot: "p",
                system_prompt_snapshot: None,
                launch_spec_json: None,
                session_id: None,
                session: None,
            })
            .unwrap();
        db.promote_run_uow(run.id, None, None, None).unwrap();
        run
    };
    finalize_run(&d, src_run.id, RunOutcome::Ok, None, None, false, true).unwrap();
    let auto = {
        let db = d.store.lock();
        db.list_runs(card_id)
            .unwrap()
            .into_iter()
            .find(|r| r.ended_at.is_none())
            .unwrap()
    };

    // --- Compare: only session_id is non-deterministic ---
    assert!(manual.session_id.is_some());
    assert!(auto.session_id.is_some());
    assert_ne!(manual.session_id, auto.session_id);

    assert_eq!(manual.harness, auto.harness);
    assert_eq!(manual.argv_json, auto.argv_json);
    assert_eq!(manual.prompt_snapshot, auto.prompt_snapshot);
    assert_eq!(manual.system_prompt_snapshot, auto.system_prompt_snapshot);
    assert_eq!(manual.session, auto.session);
    assert_eq!(manual.launch_spec, auto.launch_spec);

    // Self-consistency: argv_json must round-trip through the execution spec.
    let spec = manual.launch_spec.as_ref().unwrap();
    assert_eq!(
        &serde_json::from_str::<Vec<String>>(&manual.argv_json).unwrap(),
        &spec.execution().argv
    );
}
