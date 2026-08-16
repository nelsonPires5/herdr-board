use super::*;
use rusqlite::Connection;

#[test]
fn daemon_stop_triggers_shutdown_and_reports_stopping() {
    let d = test_daemon(Config::default());
    assert!(!d.is_shutdown());
    let res = handle_request(&d, "daemon.stop", json!({})).unwrap();
    assert_eq!(res["stopping"], true);
    assert!(d.is_shutdown());
}

#[test]
fn daemon_status_reports_supported_pingable_herdr_as_connected() {
    let herdr = testkit::herdr_server().serve();
    let client = board_herdr::HerdrClient::connect(&herdr.socket).unwrap();
    let d = testkit::daemon().herdr(client).build_daemon();

    let status = handle_request(&d, "daemon.status", json!({})).unwrap();

    assert_eq!(status["herdr_connected"], true);
    assert_eq!(herdr.methods(), vec!["ping"]);
}

#[test]
fn daemon_status_does_not_report_incompatible_pingable_herdr_as_connected() {
    let herdr = testkit::herdr_server().version("0.8.1").serve();
    let client = board_herdr::HerdrClient::connect(&herdr.socket).unwrap();
    let d = testkit::daemon().herdr(client).build_daemon();

    let status = handle_request(&d, "daemon.status", json!({})).unwrap();

    assert_eq!(status["herdr_connected"], false);
    assert_eq!(herdr.methods(), vec!["ping"]);
}

#[test]
fn daemon_status_reprobes_a_reachable_handle_after_an_initial_mismatch() {
    let probes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let probes_for_server = Arc::clone(&probes);
    let herdr = testkit::herdr_server()
        .on("ping", move |req| {
            let supported = probes_for_server.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0;
            testkit::reply(
                req,
                if supported {
                    json!({
                        "type": "pong",
                        "version": board_herdr::SUPPORTED_HERDR_VERSION,
                        "protocol": board_herdr::SUPPORTED_HERDR_PROTOCOL,
                        "capabilities": {}
                    })
                } else {
                    json!({
                        "type": "pong",
                        "version": "0.8.1",
                        "protocol": board_herdr::SUPPORTED_HERDR_PROTOCOL,
                        "capabilities": {}
                    })
                },
            )
        })
        .serve();
    // Reachability is enough to retain the default-session handle. Compatibility
    // is deliberately checked by each status operation below.
    let client = crate::initial_herdr_handle(&herdr.socket).expect("reachable Herdr handle");
    let d = testkit::daemon().herdr(client).build_daemon();

    let first = handle_request(&d, "daemon.status", json!({})).unwrap();
    let second = handle_request(&d, "daemon.status", json!({})).unwrap();

    assert_eq!(first["herdr_connected"], false);
    assert_eq!(second["herdr_connected"], true);
    assert_eq!(herdr.methods(), vec!["ping", "ping"]);
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
    assert_eq!(legacy["board"]["name"], "main");
    assert_eq!(legacy["board"]["project_id"], 1);
    let omitted = handle_request(&d, "board.get", Value::Null).unwrap();
    assert_eq!(omitted["board"]["name"], "main");
    let list = handle_request(&d, "board.list", json!({})).unwrap();
    assert_eq!(list["boards"][0]["name"], "main");
    assert_eq!(list["boards"][0]["scope_path"], Value::Null);
}

#[test]
fn board_get_snapshot_includes_archived_cards() {
    let d = test_daemon(Config::default());
    let active = handle_request(&d, "card.create", json!({"title":"active"})).unwrap();
    let archived = handle_request(&d, "card.create", json!({"title":"archived"})).unwrap();
    handle_request(
        &d,
        "card.archive",
        json!({"id": archived["id"], "archived": true}),
    )
    .unwrap();

    let cards = handle_request(&d, "board.get", json!({})).unwrap()["cards"]
        .as_array()
        .unwrap()
        .clone();
    let ids = cards
        .iter()
        .map(|card| card["id"].as_i64().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            active["id"].as_i64().unwrap(),
            archived["id"].as_i64().unwrap()
        ]
    );
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
fn template_apply_rolls_back_columns_after_intermediate_wiring_failure() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("template-atomic.db");
    let db = Db::open(&path).unwrap();
    let before = db.list_columns(BOARD_ID).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER abort_template_execute_wire
             BEFORE UPDATE OF on_success_column_id ON columns
             WHEN NEW.name='Execute'
             BEGIN SELECT RAISE(ABORT, 'fault: template wire'); END;",
        )
        .unwrap();

    let (d, mut events_rx, mut dispatch_rx) = testkit::daemon()
        .db(db)
        .db_path(path.clone())
        .socket_path(dir.path().join("board.sock"))
        .build_parts();

    let error = handle_request(
        &d,
        "template.apply",
        json!({"name":"pipeline", "board_id":BOARD_ID}),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("fault: template wire"),
        "{error}"
    );

    testkit::assert_no_effects(&mut events_rx, &mut dispatch_rx);

    drop(d);
    let reopened = Db::open(&path).unwrap();
    assert_eq!(reopened.list_columns(BOARD_ID).unwrap(), before);
    assert!(reopened.list_cards(BOARD_ID).unwrap().is_empty());
}
