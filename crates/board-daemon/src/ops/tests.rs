use super::*;
use crate::session::SessionRegistry;
use crate::settings::DaemonSettings;
use crate::spawner::LocalSpawner;
use crate::store::Store;
use board_core::config::{Config, HarnessDef};
use board_core::db::{Db, EnqueueRun, LifecycleFaultPoint};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use tokio::sync::{broadcast, mpsc, watch};

fn test_daemon(config: Config) -> Arc<Daemon> {
    test_daemon_with_registry(config, None)
}

fn test_daemon_with_registry(
    config: Config,
    session_registry: Option<SessionRegistry>,
) -> Arc<Daemon> {
    let db = Db::open_in_memory().unwrap();
    let store = Store::new(db);
    let (events_tx, _events_rx) = broadcast::channel(16);
    let (dispatch_tx, _dispatch_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    Arc::new(Daemon::new(
        store,
        config,
        DaemonSettings::default(),
        PathBuf::from("/tmp/board-test.db"),
        PathBuf::from("/tmp/board-test.sock"),
        Arc::new(LocalSpawner::new()),
        None, // no herdr
        session_registry,
        events_tx,
        dispatch_tx,
        shutdown_tx,
    ))
}

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
fn merged_invalid_updates_are_atomic_and_emit_no_event() {
    let d = test_daemon(Config::default());
    let mut events = d.events_tx.subscribe();
    let created = handle_request(
        &d,
        "card.create",
        json!({
            "title": "valid settings",
            "harness": "claude",
            "model": "sonnet",
            "effort": "high",
            "permission_mode": "manual",
            "space_kind": "new_workspace",
            "space_ref": "feature",
            "space_cwd": "/repo"
        }),
    )
    .unwrap();
    let card_id = created["id"].as_i64().unwrap();
    let _ = events.try_recv().expect("create event");

    let err = handle_request(
        &d,
        "card.update",
        json!({
            "id": card_id,
            "space_kind": "new_workspace",
            "space_cwd": null
        }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let unchanged = d.store.lock().get_card(card_id).unwrap().unwrap();
    assert_eq!(unchanged.space_ref.as_deref(), Some("feature"));
    assert_eq!(unchanged.space_cwd.as_deref(), Some("/repo"));
}

#[test]
fn invalid_column_update_keeps_dependents_and_emits_no_event() {
    let d = test_daemon(Config::default());
    let mut events = d.events_tx.subscribe();
    let created = handle_request(
        &d,
        "column.create",
        json!({
            "name": "validated",
            "harness_override": "claude",
            "model_override": "sonnet",
            "effort_override": "high",
            "permission_override": "manual"
        }),
    )
    .unwrap();
    let id = created["id"].as_i64().unwrap();
    let _ = events.try_recv().expect("create event");
    let err = handle_request(
        &d,
        "column.update",
        json!({"id": id, "harness_override": null}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(matches!(
        events.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    let unchanged = d.store.lock().get_column(id).unwrap().unwrap();
    assert_eq!(unchanged.harness_override.as_deref(), Some("claude"));
    assert_eq!(unchanged.effort_override.as_deref(), Some("high"));
}

#[test]
fn daemon_stop_triggers_shutdown_and_reports_stopping() {
    let d = test_daemon(Config::default());
    assert!(!d.is_shutdown());
    let res = handle_request(&d, "daemon.stop", json!({})).unwrap();
    assert_eq!(res["stopping"], true);
    assert!(d.is_shutdown());
}

#[test]
fn board_open_list_get_and_legacy_default_are_scoped() {
    let d = test_daemon(Config::default());
    let alpha = handle_request(&d, "board.open", json!({"scope_path":"/alpha"})).unwrap();
    let beta = handle_request(&d, "board.open", json!({"scope_path":"/beta"})).unwrap();
    let alpha_id = alpha["board"]["id"].as_i64().unwrap();
    let beta_id = beta["board"]["id"].as_i64().unwrap();
    assert_ne!(alpha_id, beta_id);
    assert_eq!(alpha["columns"].as_array().unwrap().len(), 1);

    handle_request(
        &d,
        "card.create",
        json!({"board_id":alpha_id,"title":"alpha"}),
    )
    .unwrap();
    assert_eq!(
        handle_request(&d, "board.get", json!({"board_id":alpha_id})).unwrap()["cards"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        handle_request(&d, "board.get", json!({"board_id":beta_id})).unwrap()["cards"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let legacy = handle_request(&d, "board.get", json!({})).unwrap();
    assert_eq!(legacy["board"]["name"], "Global");
    let omitted = handle_request(&d, "board.get", Value::Null).unwrap();
    assert_eq!(omitted["board"]["name"], "Global");
    let list = handle_request(&d, "board.list", json!({})).unwrap();
    assert_eq!(list["boards"][0]["name"], "Global");
}

#[test]
fn board_snapshot_active_runs_are_started_open_and_board_scoped() {
    let d = test_daemon(Config::default());
    let alpha = handle_request(&d, "board.open", json!({"scope_path":"/alpha"})).unwrap();
    let beta = handle_request(&d, "board.open", json!({"scope_path":"/beta"})).unwrap();
    let alpha_id = alpha["board"]["id"].as_i64().unwrap();
    let beta_id = beta["board"]["id"].as_i64().unwrap();
    let create = |board_id: i64, title: &str| {
        handle_request(
            &d,
            "card.create",
            json!({"board_id": board_id, "title": title}),
        )
        .unwrap()
    };
    let alpha_active = create(alpha_id, "active");
    let alpha_queued = create(alpha_id, "queued");
    let alpha_ended = create(alpha_id, "ended");
    let beta_active = create(beta_id, "other board");
    let db = d.store.lock();
    let open = |value: &Value| {
        let card_id = value["id"].as_i64().unwrap();
        let card = db.get_card(card_id).unwrap().unwrap();
        let run = db
            .enqueue_run_uow(&EnqueueRun {
                card_id: card.id,
                column_id: card.column_id,
                harness: "fake",
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
        run
    };
    let _active_run = open(&alpha_active);
    let queued_card = db
        .get_card(alpha_queued["id"].as_i64().unwrap())
        .unwrap()
        .unwrap();
    db.enqueue_run_uow(&EnqueueRun {
        card_id: queued_card.id,
        column_id: queued_card.column_id,
        harness: "fake",
        argv_json: "[]",
        prompt_snapshot: "prompt",
        system_prompt_snapshot: None,
        launch_spec_json: None,
        session_id: None,
        session: None,
    })
    .unwrap();
    let ended_run = open(&alpha_ended);
    db.finalize_run_uow(&FinalizeRun {
        run_id: ended_run.id,
        outcome: RunOutcome::Ok,
        summary: None,
        comments: &[],
        target_column_id: None,
        final_status: CardStatus::Done,
        final_awaiting_reason: None,
        next: None,
    })
    .unwrap();
    let _other_run = open(&beta_active);
    drop(db);

    let snapshot = handle_request(&d, "board.get", json!({"board_id": alpha_id})).unwrap();
    assert_eq!(snapshot["active_runs"].as_array().unwrap().len(), 1);
    assert_eq!(snapshot["active_runs"][0]["card_id"], alpha_active["id"]);
    assert!(snapshot["active_runs"][0]["started_at"].is_string());
}

#[test]
fn template_and_scheduler_operate_on_scoped_board() {
    let d = test_daemon(Config::default());
    let opened = handle_request(&d, "board.open", json!({"scope_path":"/scoped"})).unwrap();
    let board_id = opened["board"]["id"].as_i64().unwrap();
    handle_request(
        &d,
        "template.apply",
        json!({"name":"pipeline","board_id":board_id}),
    )
    .unwrap();
    let snapshot = handle_request(&d, "board.get", json!({"board_id":board_id})).unwrap();
    assert_eq!(snapshot["columns"].as_array().unwrap().len(), 6);
    let execute = snapshot["columns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|column| column["name"] == "Execute")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let card = handle_request(
        &d,
        "card.create",
        json!({"board_id":board_id,"column_id":execute,"title":"queued","harness":"pi"}),
    )
    .unwrap();
    assert_eq!(card["board_id"], board_id);
    assert!(d
        .store
        .queued_runs()
        .unwrap()
        .iter()
        .any(|(_, queued)| queued.id == card["id"].as_i64().unwrap()));
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

fn add_run_with_pane(d: &Arc<Daemon>, pane: Option<&str>) -> i64 {
    let db = d.store.lock();
    let card = db
        .create_card(&CardCreateParams {
            title: "focus target".into(),
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
    db.promote_run_uow(run.id, Some("w1"), pane, None).unwrap();
    card.id
}

fn fake_herdr(reply: &'static str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("herdr.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let stream = incoming.unwrap();
            let mut writer = stream.try_clone().unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 {
                continue;
            }
            let request: Value = serde_json::from_str(line.trim()).unwrap();
            assert_eq!(request["method"], "pane.focus");
            let id = request["id"].as_str().unwrap();
            writeln!(writer, "{{\"id\":\"{id}\",{reply}}}").unwrap();
            break;
        }
    });
    (dir, path)
}

#[test]
fn run_focus_rejects_missing_pane_and_cross_session_socket() {
    let d = test_daemon(Config::default());
    let card_id = add_run_with_pane(&d, None);
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"origin_socket":"/tmp/origin.sock"}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 2);
    assert!(err.to_string().contains("pane"));

    let target_dir = tempfile::tempdir().unwrap();
    let target = target_dir.path().join("target.sock");
    let _listener = UnixListener::bind(&target).unwrap();
    let origin_dir = tempfile::tempdir().unwrap();
    let origin = origin_dir.path().join("origin.sock");
    let _origin_listener = UnixListener::bind(&origin).unwrap();
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::new(target.clone())),
    );
    let card_id = add_run_with_pane(&d, Some("w1:p2"));
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"origin_socket":origin}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 3);
    assert!(err.to_string().contains("different Herdr session"));
}

#[test]
fn run_focus_propagates_herdr_error_and_returns_success_ids() {
    let (_dir, socket) = fake_herdr("\"error\":{\"code\":\"pane_not_found\",\"message\":\"gone\"}");
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::new(socket.clone())),
    );
    let card_id = add_run_with_pane(&d, Some("w1:p9"));
    let err = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"origin_socket":socket}),
    )
    .unwrap_err();
    assert_eq!(err.code(), 4);
    assert!(err.to_string().contains("gone"));

    let (_dir, socket) = fake_herdr(
            "\"result\":{\"type\":\"pane_info\",\"pane\":{\"pane_id\":\"w1:p9\",\"terminal_id\":\"term\",\"workspace_id\":\"w1\",\"tab_id\":\"w1:t1\",\"focused\":true,\"revision\":0,\"agent_status\":\"idle\"}}",
        );
    let d = test_daemon_with_registry(
        Config::default(),
        Some(SessionRegistry::new(socket.clone())),
    );
    let card_id = add_run_with_pane(&d, Some("w1:p9"));
    let result = handle_request(
        &d,
        "run.focus",
        json!({"card_id":card_id,"origin_socket":socket}),
    )
    .unwrap();
    assert_eq!(result["pane_id"], "w1:p9");
    assert!(result["run_id"].as_i64().unwrap() > 0);
}

#[test]
fn harness_list_builtin_only() {
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.list", json!({})).unwrap();
    let names: Vec<String> = serde_json::from_value(v["harnesses"].clone()).unwrap();
    assert_eq!(names, vec!["pi".to_string(), "claude".to_string()]);
}

#[test]
fn harness_list_includes_config_defined() {
    let mut config = Config::default();
    config.harness.insert(
        "fake".to_string(),
        HarnessDef {
            argv: vec!["bash".into(), "fake.sh".into()],
            ..Default::default()
        },
    );
    let d = test_daemon(config);
    let v = handle_request(&d, "harness.list", json!({})).unwrap();
    let names: Vec<String> = serde_json::from_value(v["harnesses"].clone()).unwrap();
    assert_eq!(names, vec!["pi", "claude", "fake"]);
}

#[test]
fn harness_capabilities_claude_ok() {
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "claude" })).unwrap();
    assert_eq!(v["harness"], "claude");
    assert_eq!(v["model_freeform"], true);
    assert!(v["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == "sonnet"));
}

#[test]
fn harness_capabilities_pi_ok() {
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "pi" })).unwrap();
    assert_eq!(v["harness"], "pi");
    assert_eq!(v["model_freeform"], true);
    assert!(v["models"].as_array().unwrap().is_empty());
    assert!(v["permission_modes"].as_array().unwrap().is_empty());
    assert!(v["default_efforts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|effort| effort == "low"));
}

#[test]
fn harness_capabilities_pi_overlays_live_catalog() {
    // A pi agent dir with auth.json + models-store.json → the daemon
    // overlays real models (per-model efforts) onto the pi catalog.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("auth.json"),
        r#"{"zai": {"type": "api_key"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("models-store.json"),
        r#"{"zai": {"models": [{"id": "glm-5.2", "reasoning": true,
                 "thinkingLevelMap": {"minimal": "low", "xhigh": "xhigh"}}]}}"#,
    )
    .unwrap();
    let config = Config {
        pi_agent_dir: Some(dir.path().to_path_buf()),
        ..Config::default()
    };
    let d = test_daemon(config);

    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "pi" })).unwrap();
    let models = v["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["id"], "zai/glm-5.2");
    // Per-model efforts come from thinkingLevelMap, in canonical order.
    let efforts: Vec<&str> = models[0]["efforts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap())
        .collect();
    assert_eq!(efforts, vec!["minimal", "xhigh"]);
    // model_freeform stays true: arbitrary model strings are still accepted.
    assert_eq!(v["model_freeform"], true);
}

#[test]
fn harness_capabilities_pi_falls_back_to_static_without_agent_dir() {
    // No pi_agent_dir (tests) → static free-form catalog (models: []).
    let d = test_daemon(Config::default());
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "pi" })).unwrap();
    assert!(v["models"].as_array().unwrap().is_empty());
}

#[test]
fn harness_capabilities_config_defined_ok() {
    let mut config = Config::default();
    config.harness.insert(
        "fake".to_string(),
        HarnessDef {
            argv: vec!["bash".into(), "fake.sh".into()],
            models: vec!["m1".into()],
            efforts: vec!["low".into()],
            permission_modes: vec!["auto".into()],
        },
    );
    let d = test_daemon(config);
    let v = handle_request(&d, "harness.capabilities", json!({ "harness": "fake" })).unwrap();
    assert_eq!(v["harness"], "fake");
    assert_eq!(v["permission_modes"][0], "auto");
}

#[test]
fn harness_capabilities_unknown_is_not_found() {
    let d = test_daemon(Config::default());
    let err =
        handle_request(&d, "harness.capabilities", json!({ "harness": "ghost" })).unwrap_err();
    assert_eq!(err.code(), 2);
    let msg = err.to_string();
    assert!(msg.contains("ghost"), "message: {msg}");
    assert!(msg.contains("pi"), "message lists Pi: {msg}");
    assert!(msg.contains("claude"), "message lists Claude: {msg}");
}

#[test]
fn card_create_rejects_pi_permission_mode() {
    let d = test_daemon(Config::default());
    let err = handle_request(
        &d,
        "card.create",
        json!({ "title": "bad", "harness": "pi", "permission_mode": "acceptEdits" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    assert!(err.to_string().contains("permission mode"));
}

#[test]
fn switching_card_to_pi_rejects_incompatible_permission() {
    let d = test_daemon(Config::default());
    let created = handle_request(
        &d,
        "card.create",
        json!({
            "title": "switch",
            "harness": "claude",
            "permission_mode": "acceptEdits"
        }),
    )
    .unwrap();
    let err = handle_request(
        &d,
        "card.update",
        json!({ "id": created["id"], "harness": "pi" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    let unchanged = d
        .store
        .lock()
        .get_card(created["id"].as_i64().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.harness, "claude");
    assert_eq!(unchanged.permission_mode.as_deref(), Some("acceptEdits"));
}

#[test]
fn switching_card_from_pi_to_claude_rejects_incompatible_effort() {
    let d = test_daemon(Config::default());
    let created = handle_request(
        &d,
        "card.create",
        json!({ "title": "switch", "harness": "pi", "effort": "off" }),
    )
    .unwrap();
    let err = handle_request(
        &d,
        "card.update",
        json!({ "id": created["id"], "harness": "claude" }),
    )
    .unwrap_err();
    assert_eq!(err.code(), 1);
    let unchanged = d
        .store
        .lock()
        .get_card(created["id"].as_i64().unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(unchanged.harness, "pi");
    assert_eq!(unchanged.effort, Some(Effort::Off));
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

#[test]
fn space_list_without_herdr_is_herdr_unavailable() {
    let d = test_daemon(Config::default());
    let err = handle_request(&d, "space.list", json!({})).unwrap_err();
    assert_eq!(err.code(), 4);
}
